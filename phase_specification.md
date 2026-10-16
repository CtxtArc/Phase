# PHASE — Complete Specification

**A physical-domain safety language for signal, RF, and embedded pipelines**

Version 2.3

---

## Table of Contents

1. [Project Definition & Scope](#part-1-project-definition--scope)
2. [Physical Domain Model](#part-2-physical-domain-model)
3. [Language Reference](#part-3-language-reference)
4. [Type System & Static Analysis](#part-4-type-system--static-analysis)
5. [Runtime & Simulated Hardware](#part-5-runtime--simulated-hardware)
6. [Compiler Architecture](#part-6-compiler-architecture)
7. [PIR — Phase Intermediate Representation](#part-7-pir--phase-intermediate-representation)
8. [Bug Gallery — What the Compiler Must Catch](#part-8-bug-gallery--what-the-compiler-must-catch)
9. [Implementation Roadmap & Milestones](#part-9-implementation-roadmap--milestones)
10. [Non-Goals](#part-10-non-goals)

---

<a name="part-1-project-definition--scope"></a>
## Part 1: Project Definition & Scope

### 1.1 One-sentence definition

PHASE is a small, standalone, compiled language whose type system proves that
buffers moving between physically distinct execution domains (ADC/DAC front
ends, DMA engines, MMIO-mapped devices, host RAM) are never accessed by the
wrong domain at the wrong time.

### 1.2 The problem it solves

In embedded, RF, and driver-adjacent code, a large and recurring class of bugs
and CVEs comes from exactly one pattern: **a buffer is read or written by a
domain that does not currently own it.**

Examples of this pattern in the wild:

- CPU reads a DMA buffer before the transfer engine has finished writing it.
- A device is handed a buffer, the buffer is freed or reused on the host
  side before the device is done with it (use-after-free across a domain
  boundary).
- An MMIO register is read/written without accounting for side effects,
  and the compiler "optimizes away" an access that mattered.
- A packet is parsed/decoded before the underlying transfer/validation step
  has actually completed (TOCTOU across a boundary).

C does not track any of this. Rust's borrow checker tracks aliasing *within
one execution domain* but has no concept of "this memory is currently owned
by a DMA engine." PHASE's entire reason to exist is to make this specific bug
class a compile-time error instead of a runtime incident.

### 1.3 What PHASE is NOT trying to be

- Not a general-purpose systems language (no ambition to replace C/Rust/Zig).
- Not a DSP language. PHASE does not know what an FFT or FIR filter is — it
  calls out to existing, trusted C/Rust kernels for the actual math and only
  reasons about *when* it is legal to touch the buffers those kernels operate
  on.
- Not a GPU/FPGA compiler. Accelerators are modeled as a generic `DEVICE`
  domain with the same handoff rules as everything else; specialized
  GPU/FPGA support is explicitly future work.
- Not tied to real silicon. The default target is simulated hardware
  running on the host, so the project is fully testable without an SDR or a
  microcontroller; a real ARM Cortex-M3 runtime exists alongside it (§5.3).

### 1.4 Why this is a good portfolio compiler project

- It has a genuine, novel static-analysis core (a domain-aware borrow
  checker) — not a rehash of a textbook toy language.
- It maps directly onto a real, nameable bug class (embedded/driver
  use-after-free and TOCTOU bugs), which gives the project a concrete
  security narrative, not just "look, a compiler."
- It is scoped so every milestone produces something runnable and
  demoable, rather than requiring the whole system to exist before anything
  works.
- Compiling to readable C (not LLVM) for v1 keeps 100% of the engineering
  effort on the actually-novel part of the project (the physical analysis)
  instead of backend plumbing.

---

<a name="part-2-physical-domain-model"></a>
## Part 2: Physical Domain Model

### 2.1 Domains (v1 fixed set)

PHASE v1 supports exactly five domains. This is intentionally small — the
whole point is depth, not breadth.

| Domain    | Meaning                                             | CPU-readable by default? |
|-----------|------------------------------------------------------|---------------------------|
| `@RAM`    | Ordinary host memory                                  | yes                       |
| `@STACK`  | Function-local memory                                 | yes                       |
| `@DMA`    | Memory owned/transferred by a DMA engine              | no — requires `sync`      |
| `@MMIO`   | Memory-mapped hardware register                       | yes, but every access is an observable side effect |
| `@DEVICE` | Generic external domain (ADC, DAC, radio front end, accelerator, network interface) | no — requires `sync` or explicit handoff |

`@DEVICE` is deliberately generic in v1. A real ADC, a real GPU, and a real
network card all behave the same way from the *safety* point of view: you
hand them a buffer, they own it for a while, you get it back after a
synchronization point. Specializing `@DEVICE` into named subtypes
(`@GPU`, `@RADIO`, `@NIC`) is future work and does not require changing the
core analysis.

### 2.2 Entity model

Every value that occupies a domain is an **entity**:

```
Entity =
    Identity        (unique id)
    + Type           (element type, e.g. i16, Sample<f32>)
    + Domain         (@RAM, @DMA, @MMIO, @DEVICE)
    + Ownership State (Owned, Borrowed, DeviceOwned, Destroyed)
    + History        (sequence of transitions it has undergone)
```

### 2.3 Ownership states

```
Owned          -- normal, accessible per domain rules
Borrowed(R|W)  -- temporarily lent out; original owner cannot conflict
DeviceOwned    -- handed to @DMA or @DEVICE; host cannot access until synced
Destroyed      -- lifetime ended; any further access is an error
```

### 2.4 Legal transitions (v1 table)

```
@STACK  --move-->        @RAM
@RAM    --move-->        @DMA        (buffer becomes DeviceOwned)
@DMA    --sync-->        @RAM        (buffer becomes Owned again, CPU-visible)
@RAM    --move-->        @DEVICE     (buffer becomes DeviceOwned)
@DEVICE --sync-->        @RAM        (buffer becomes Owned again)
@RAM    --borrow(R|W)--> @RAM        (temporary, no domain change)
@MMIO   --volatile_read-->  @RAM     (value only, not the register itself)
@RAM    --volatile_write--> @MMIO
```

Everything not in this table is a compile error: **"no legal transition from
X to Y."** This table is intentionally a flat, hardcoded structure in v1
(see §6.6) — not a plugin system yet. Making it pluggable is future work.

### 2.5 Costs and observability

Every transition has a declared cost class used only for diagnostics/tracing
in v1 (not for optimization yet):

```
move            -- cheap, same-domain-family
sync            -- potentially blocking, waits on hardware
volatile access -- always observable, never eliminated by codegen
```

---

<a name="part-3-language-reference"></a>
## Part 3: Language Reference

### 3.1 Lexical elements

Identifiers, integer/float literals, string literals, comments (`//`, `/* */`).
Keywords: `entity`, `fn`, `extern`, `let`, `buffer`, `move`, `sync`, `borrow`,
`release`, `destroy`, `swap`, `rotate`, `split`, `join`, `into`, `read`,
`write`, `state`, `if`, `else`, `while`,
`return`, `true`, `false`.

### 3.2 Entities and domains

```phase
entity Sample : @RAM {
    value: f32
}

fn main() {
    let s: Sample @RAM = Sample { value: 0.0 };
}
```

### 3.3 Buffers

```phase
buffer<Sample, 1024> radio_buf @DMA;
```

A `buffer<T, N>` is the core aggregate type — a fixed-size, domain-tagged
array of entities. This is the primary object DMA/DEVICE code manipulates.

**Type checking.** A buffer passed to a function must match the
parameter's element type and length exactly; a borrow has its target's
type. (Scalars convert as in C; entity and typestate arguments must have
the parameter's base type.)

**Length-generic parameters.** In a *parameter* type the length may be
`_`:

```phase
extern fn summarize(x: read buffer<Sample, _> @RAM);
fn wait_capture(b: buffer<Sample, _> @DMA -> @RAM) { sync(b); }
```

Such a parameter accepts a buffer of any length. In C every buffer
parameter already comes with a `<name>_len` argument; for a `_` parameter
that run-time value is what gets passed on to callees and used by `sync`,
and a borrow of it gets a `_len` of its own. A run-time length only fits
another `_` parameter: passing one where a fixed length is required is an
error, since nothing guarantees it matches. `_` is not allowed where
storage is declared (locals, entity fields, return types).

### 3.4 move

```phase
move radio_buf -> @DMA;
```

Transfers ownership of `radio_buf` from its current domain to `@DMA`. After
this line, `radio_buf` is `DeviceOwned` and any CPU access is a compile
error until it is synced back.

### 3.5 sync

```phase
sync(radio_buf);
```

Makes a `DeviceOwned` entity `Owned` and CPU-visible again. Lowers to a
runtime wait on the simulated (or real) transfer engine.

### 3.6 borrow / release

```phase
let view = borrow radio_buf read;
// ... read-only access ...
release view;
```

Multiple concurrent `read` borrows are legal. A `write` borrow must be
exclusive (no other borrows of any kind may be live). Attempting a `move` or
`destroy` while a borrow is live is a compile error. While only `read`
borrows are live, the owner may still be read — passed to a `read`
parameter (§3.12) or have its fields read — but not written.

### 3.7 Devices, registers and volatile MMIO access

A `device` declares a peripheral's register map (v2.0):

```phase
device Uart0 @ 0x40004000 {
    DATA:    read write u32 @ 0x00;
    STATE:   read u32 @ 0x04 { TXFULL: bit 0, RXFULL: bit 1 }
    CTRL:    read write u32 @ 0x08 = 0 { TX_EN: bit 0, RX_EN: bit 1 }
    BAUDDIV: read write u32 @ 0x10 { DIV: bits 0..=19 }
}

fn send_byte(c: u32) {
    while volatile_read(Uart0.STATE.TXFULL) == 1 {
    }
    volatile_write(Uart0.DATA, c);
}
```

The device has a base address; each register has an access mode (`read`,
`write`, or both), a type (`u8`, `u16` or `u32`), a byte offset, an
optional reset value (`= 0`), and optional fields — `bit N`, or
`bits LO..=HI`, inclusive, lowest bit first. Addresses may be written in
hex (`0x4000_4000`).

`volatile_read(Dev.REG)` reads a whole register and
`volatile_read(Dev.REG.FIELD)` one field, shifted down to bit 0;
`volatile_write(Dev.REG, v)` writes a whole register and
`volatile_write(Dev.REG.FIELD, v)` one field, leaving the other bits as
they were. The analyzer checks:

- **The map itself.** A register is `u8`, `u16` or `u32`, aligned to its
  size, and overlaps no other register; a field lies inside its register
  and overlaps no other field; a reset value fits; names are unique, and
  a device's name is not also a function, entity, typestate or local.
- **Names.** A misspelled register or field is an error that lists the
  ones that exist.
- **Access.** Only a readable register is read and only a writable one
  written. Writing a *field* also needs a readable register: it is a
  read-modify-write, and a write-only register reads back as something
  other than what was last written (#36).
- **Values.** A literal written to a register or field must fit its width
  (#37). A non-literal value is masked to the field's width.

A register is a hardware location, not a value: `Uart0.STATE` on its own
is an error, as is using `Uart0` as a value.

Codegen lays each device out as a C struct of `volatile` members at the
declared offsets (each one checked by `_Static_assert`), with
`Dev_REG_FIELD_Pos`/`_Msk` macros so hand-written C can use the same
fields. On a hardware target (`--target=mps2-an385`, §5.3) the device
macro is the declared address; in a host build it is a simulated register
block that starts from the declared reset values. A field write is a
read-modify-write and is not atomic with respect to an interrupt handler
writing the same register.

**Device types (v2.1).** Several copies of one peripheral share a layout:

```phase
device type CmsdkUart {
    DATA:  read write u32 @ 0x00;
    STATE: read u32 @ 0x04 { TXFULL: bit 0 }
}
device Uart0: CmsdkUart @ 0x40004000;
device Uart1: CmsdkUart @ 0x40005000;
```

A `device type` is a register map without an address, validated like a
device's; `device NAME: TYPE @ BASE;` is a device with that type's
registers. An instance must name a declared type. For every device, the
base address must keep its registers aligned (`BASE` a multiple of its
widest register), and no two devices may overlap in the address space
(#40) — the extent of a device runs from its base to the end of its last
register. In C, a type is one struct (`phase_devtype_T_regs`, with
`T_REG_FIELD_Pos`/`_Msk` macros), and each of its devices a pointer of
that type. `examples/devices/mps2_an385.phase` declares the MPS2 AN385
board's UARTs and timers this way, for programs to `import` (§3.15).

The pre-2.0 form, a bare register name tagged `@MMIO`, is still accepted
and becomes a simulated `uint32_t` register:

```phase
let status: u32 = volatile_read(UART_STATUS @MMIO);
volatile_write(UART_CONTROL @MMIO, 0x1);
```

Either way, volatile accesses are never eliminated, reordered across each
other, or cached by codegen, regardless of optimization level.

### 3.8 Typestate (protocol states)

```phase
state Packet {
    Received -> Decoded -> Validated
}

fn decode(p: Packet<Received>) -> Packet<Decoded> { ... }
```

A function that requires `Packet<Decoded>` cannot be called with a
`Packet<Received>` — this is checked the same way a domain mismatch is: at
compile time, by the state analyzer described in §4.

### 3.9 Functions and extern kernels

```phase
extern fn fir_filter(input: buffer<Sample, 1024> @RAM,
                      output: buffer<Sample, 1024> @RAM);
```

`extern fn` declares a trusted, opaque operation (typically a C function)
that PHASE does not analyze internally — it only checks that the domains of
the arguments match the declared signature at the call site. This is how DSP
math re-enters the pipeline without PHASE needing to understand it.

A parameter's domain is what the argument must be in when the function is
called; unless the parameter declares a change (§3.11), the function hands
it back in that same domain.

### 3.10 Example — a minimal end-to-end pipeline

```phase
extern fn fir_filter(in_: buffer<Sample, 1024> @RAM,
                      out: buffer<Sample, 1024> @RAM);

fn main() {
    buffer<Sample, 1024> raw @DMA;
    device_capture(raw);       // simulated ADC fills the DMA buffer
    sync(raw);                 // wait for capture to finish; entity is now @RAM

    buffer<Sample, 1024> filtered @RAM;
    fir_filter(raw, filtered);

    move filtered -> @DEVICE;  // hand to simulated DAC
    device_playback(filtered);
    sync(filtered);
}
```

### 3.11 Domain changes in signatures

```phase
extern fn start_capture(dst: buffer<Sample, 256> @RAM -> @DMA);

fn wait_capture(b: buffer<Sample, 256> @DMA -> @RAM) {
    sync(b);
}

fn main() {
    buffer<Sample, 256> frame @RAM;
    start_capture(frame);   // frame is now @DMA
    make_window(window);    // CPU work while the transfer runs
    wait_capture(frame);    // frame is @RAM again
}
```

A parameter written `name: T @FROM -> @TO` declares that the function
changes the argument's domain:

- **Signature.** `FROM -> TO` must be a legal transition (§2.4): a `move`
  (`@STACK -> @RAM`, `@RAM -> @DMA`, `@RAM -> @DEVICE`) or a `sync`
  (`@DMA -> @RAM`, `@DEVICE -> @RAM`). A change that changes nothing
  (`@RAM -> @RAM`) is rejected.
- **Call site.** The argument must be in `FROM` and must be an owner, not
  a borrow (a borrow cannot hand the storage it views to hardware). After
  the call the argument is in `TO`, exactly as if the caller had written the
  `move`/`sync` itself.
- **Body** (for a PHASE `fn`). The parameter starts in `FROM` and must be
  in `TO` at every `return`, explicit or falling off the end.
- **`extern fn`.** The change is part of the trusted contract (§4.4): an
  extern declared `@RAM -> @DMA` is trusted to have started a transfer, one
  declared `@DMA -> @RAM` to have waited for it.

This is what lets a pipeline split "start the transfer" and "wait for it"
into separate functions with CPU work in between — the overlap DMA exists
for — while the checker still tracks the buffer through both calls. See
`examples/overlapped_capture.phase`.

### 3.12 Access modes on parameters

```phase
extern fn fir_filter(in_: read buffer<Sample, 1024> @RAM,
                      out: write buffer<Sample, 1024> @RAM);
extern fn inspect(p: read Packet<Validated>);
```

`read` or `write` before a parameter's type says how the function accesses
the buffer or entity it is given. A parameter with neither (the pre-1.2
form) is assumed to possibly write.

- **Read-only data stays read-only.** A read borrow, or a `read`
  parameter inside its function, may only be passed to `read` parameters.
  Passing it to a `write` parameter, or to a parameter with no mode, is an
  error (#39). (Before 2.0 the second case was a warning, so 1.1 programs
  kept compiling.) Data the caller may write can still go to a parameter
  with no mode.
- **Inside a PHASE function** a `read` parameter cannot be moved, synced,
  destroyed, or write-borrowed.
- **Aliasing.** The same storage may appear more than once in a call when
  every occurrence is read-only — a `read` parameter or a read borrow — so
  `cmp(x, x)` is fine and `fir_filter(x, x)` is not.
- **Borrows.** While only read borrows of an entity are live, the owner may
  be passed to a `read` parameter.
- **Typestate.** A `read` parameter that requires a state checks it but
  does not consume the value: `inspect(v); handle(v);` is legal.
- **Signatures.** Modes apply only to buffers and entities, and `read`
  cannot be combined with a domain change (§3.11): handing a buffer to
  hardware lets the hardware write it.
- **Codegen.** A `read` buffer parameter is a `const` pointer in C, so the C
  compiler rejects an extern body that writes through it — the first part
  of the `extern fn` contract (§4.4) that is machine-checked.

### 3.13 swap and double buffering

```phase
start_capture(ping);            // ping in flight
while n < 6 {
    wait_capture(ping);         // ping: CPU-visible
    start_capture(pong);        // pong: next frame in flight
    process(ping);
    swap ping, pong;            // ping is in flight again
    n = n + 1;
}
wait_capture(ping);
```

`swap a, b;` exchanges the storage two buffer names refer to, together
with everything the checker knows about it (domain, history). Both must be
live, owned, unborrowed local buffers of the same type, declared in the
same block — not borrow handles, `read` parameters, or parameters at all
(their storage belongs to the caller). The same-block rule matters because
storage moves with the name: swapping with a buffer from an inner block
would leave the outer name pointing at storage that dies when the block
ends.

It exists for ping-pong double buffering, the standard way to stream from
a DMA engine: one buffer is filled by hardware while the CPU processes the
other. Without `swap` that loop's body ends with the two buffers in each
other's starting domains, which the loop invariant (§4.3) rejects; with
it, the body leaves `ping` and `pong` exactly as it found them, so the
single-pass loop check remains sound. In C, a swapped buffer is a storage
array plus a pointer, and `swap` exchanges the pointers. See
`examples/ping_pong.phase`.

### 3.14 rotate, split and join

Two more buffer shapes come up in every streaming driver: more than two
buffers in a ring, and one buffer the DMA engine fills a half at a time.

**`rotate a, b, c;`** generalizes `swap` to any number of buffers: each
name takes the *next* one's storage (`a` ← `b` ← `c` ← `a`), with
everything the checker knows about it. The rules are `swap`'s: live,
owned, unborrowed local buffers of one type, declared in one block, each
named once. `swap a, b;` is `rotate a, b;`. Triple buffering keeps two
captures in flight while the CPU works on a third, and `rotate` is what
lets its loop body end exactly as it began (`examples/triple_buffer.phase`):

```phase
start_capture(a); start_capture(b);   // a, b in flight; c free
while n < 6 {
    wait_capture(a);                  // the oldest frame is ready
    start_capture(c);                 // keep two in flight
    process(a);
    rotate a, b, c;                   // a, b in flight; c free -- as on entry
    n = n + 1;
}
```

Rotated the wrong way, the loop would next wait on a buffer nothing is
filling; the loop invariant rejects it (#33).

**`split buf into p0, p1, …;`** cuts a buffer into equal, consecutive
slices and makes each one a separately tracked buffer: part *i* is
`buffer<T, len/n>`, starts in `buf`'s domain, and from then on moves,
syncs, borrows and is passed to calls on its own. The parent must be an
owned local (not a parameter — its storage belongs to the caller) with a
fixed length divisible by *n* ≥ 2, unborrowed. While split, the parent
cannot be used at all — not passed, moved, borrowed, swapped or
destroyed (#31) — and a part cannot be destroyed on its own.

**`join p0, p1, … into buf;`** ends the split. It names exactly the parts
`buf` was split into, in order, and each must still hold its own slice: a
part that was swapped or rotated away has to be swapped back first. The
parts must be live, unborrowed, and all in one domain (#32), which `buf`
then has; the part names are consumed. A buffer may stay split until it
goes out of scope — its parts carry the block-end and exit obligations —
but it must be split, or not, on every path into a merge, and a `while`
body that splits must join before it ends.

This is what circular DMA with half-transfer interrupts needs
(`examples/circular_dma.phase`): the engine runs over the whole buffer
forever, and the CPU may touch only the half it just finished.

```phase
split ring into lo, hi;
start_circular(lo, hi);               // the engine owns both halves
while n < 4 {
    wait_half(lo); process(lo); requeue(lo);   // engine is filling hi
    wait_half(hi); process(hi); requeue(hi);   // engine wrapped to lo
    n = n + 1;
}
stop_circular(lo, hi);
join lo, hi into ring;
summarize(ring);                      // the whole buffer is the CPU's again
```

Processing `hi` right after the half-transfer interrupt — the classic
off-by-one-half bug in circular ADC drivers — is #30. In C, parts are
pointers into the parent's storage (`lo = ring + 0; hi = ring + 64;`);
`rotate` moves pointers like `swap`, and `join` emits nothing.

### 3.15 Programs across files

```phase
import "devices/mps2_an385.phase";
```

`import "PATH";` (v2.1) makes every item of that file part of the
program: its functions, externs, entities, typestate machines, devices
and device types. `PATH` is relative to the directory of the file that
imports it. A file is loaded once however many files import it, so
imports may form cycles; there is one namespace, so two files declaring
the same name is the same error as one file doing it twice. `main` must
be in the file being compiled. `import` is a keyword only at the start of
an item.

Every source location records its file, so an error inside an imported
file is reported in that file, and a note can point into any of them
(§6.5). An `extern fn` declared in an imported file is implemented like
any other: in `runtime/<stem>_extern.c` of the program being built.

### 3.16 Interrupts

```phase
event tick;

interrupt fn on_timer0() @ irq 8 {
    volatile_write(Timer0.INT.PENDING, 1);   // acknowledge
    signal tick;
}

fn main() {
    ...
    wait tick;
    critical {
        volatile_write(Timer0.CTRL.ENABLE, 0);
    }
}
```

(v2.2.) `interrupt fn NAME() @ irq N { .. }` is the handler for external
interrupt `N` (0–239 on a Cortex-M): the hardware calls it, between any
two instructions of the code it interrupts. It takes no parameters and
returns nothing, it is never called directly, and each IRQ has at most
one handler. `event NAME;` declares an event: `signal E;` raises it, and
`wait E;` sleeps until it is raised, then clears it. An event is a flag,
not a counter — two signals before a `wait` are seen as one.
`critical { .. }` runs its block with interrupts disabled, restoring the
previous state at its end. `interrupt`, `event`, `signal`, `wait` and
`critical` are keywords only where they start an item or a statement, so
`fn wait(..)` and `wait(x);` are unaffected.

*Thread code* is `main` and every function it calls; *handler code* is
every handler and every function a handler calls (one function can be
both). The analyzer follows the call graph to check:

- **No lost register writes.** A field write is read-modify-write. In
  thread code outside `critical`, a field write to a register that handler
  code also writes is an error (#41): the handler may run between the read
  and the write, and its write would be lost. A whole-register write, a
  field write inside `critical`, and a field write in handler code (which
  isn't interrupted by other handlers here) are fine.
- **No waiting where nothing can signal.** `wait` in handler code never
  returns (#42): the interrupt that would signal it can't run until the
  handler returns. Neither does `wait` inside `critical`.
- **No early exit from `critical`.** A `return` inside it would leave
  interrupts disabled.

- **No wait that never returns.** A `wait E` where no thread or handler
  code signals `E` is an error (#44). (A `signal` that exists but is never
  executed at run time can't be seen statically; a host build stops such a
  program, below.)

The model assumes one core and handlers that don't preempt each other
(one priority level).

**Interrupts that hand over buffers (v2.3).** `wait E for b;` waits for
`E` like `wait E;` and then hands `b` back to the CPU, like `sync(b)`:
the event is the hardware saying it is done with `b` — a DMA engine's
half-transfer interrupt saying the first half of a circular buffer is
full:

```phase
interrupt fn dma0_irq() @ irq 11 {
    if volatile_read(Dma0.STATUS.HT) == 1 { volatile_write(Dma0.CLEAR, 1); signal half_done; }
    if volatile_read(Dma0.STATUS.TC) == 1 { volatile_write(Dma0.CLEAR, 2); signal full_done; }
}
...
while n < 4 {
    wait half_done for lo;   // the engine is filling hi now
    process(lo);
    move lo -> @DMA;         // back to the engine before it wraps around
    wait full_done for hi;
    process(hi);
    move hi -> @DMA;
    n = n + 1;
}
```

`b` must be owned by the hardware (`@DMA` or `@DEVICE`) at the `wait`,
exactly as for `sync`; afterwards it is `@RAM`. Within one function, every
`wait E for ..` names the same buffer: an event means one particular
transfer finished. `wait half_done for hi` after `wait half_done for lo`
is an error (#43) — the classic wrong-half bug, which ownership alone
can't see when both halves are with the engine. The rule is per function
and by name: the first `wait E for b` in a function decides which buffer
`E` gives back there, so swapping the events consistently throughout a
function is not caught. Handlers still have no access to `main`'s buffers
(no parameters, no globals): the hand-off is a promise the hardware and
the handler keep — that `E` is signalled when the transfer of `b` is
done — like the promise behind `sync`.

In C, an event is a `volatile bool`; `critical` saves PRIMASK and executes
`cpsid i`, restoring PRIMASK at the end; `wait` tests and clears its event
with interrupts disabled and sleeps with `wfi` in between (which wakes on
a pending interrupt even while they are disabled), so a signal can't slip
in between the test and the sleep. `wait E for b` adds nothing at run
time: the hand-off is ownership the compiler tracks. Each handler becomes
`IRQn_Handler`, replacing a weak entry of the vector table
(`runtime/hw/startup.c`). A host build has no interrupt sources: there,
sleeping first calls `phase_sim_hardware()` — if the program's C side
defines it, to play the hardware: set the status bits the handlers test
(`runtime/circular_dma_extern.c` simulates a circular DMA engine this
way) — and then runs every handler once, in IRQ order, as if each one's
interrupt had just arrived. A `wait` that 1,000 such rounds don't satisfy
stops the program with a message rather than hanging it.

---

<a name="part-4-type-system--static-analysis"></a>
## Part 4: Type System & Static Analysis

### 4.1 The extended type

```
PType = { BaseType, Domain, OwnershipState, TypestateLabel? }
```

Two values are only interchangeable if all four components match (or the
mismatch is resolved by an explicit `move`/`sync`/state-transition function).

### 4.2 Analysis passes (matches compiler pipeline in Part 6)

1. **Domain checker** — every access is checked against the entity's current
   domain and the legal-transition table (§2.4).
2. **Ownership/borrow checker** — enforces: many-readers/one-writer; no
   access while `DeviceOwned`; no `move`/`destroy` while borrowed.
3. **State checker (typestate)** — enforces declared state machines (§3.8);
   a call requiring state `S` on an entity currently in state `S'` is
   rejected unless a transition function `S' -> S` exists and was invoked.
4. **Lifetime checker** — no use after `destroy`; no entity escapes the
   scope that owns it without an explicit `move`.

### 4.3 Control flow and merging

At a branch/join point, an entity may have different domains/states on
different incoming paths (e.g. synced on one branch, not on the other). The
merge point requires all incoming states to agree; if they don't, this is a
compile error requiring the programmer to reconcile it (e.g. insert a `sync`
on the branch that's missing one). This is the direct analogue of KAIROS's
`ENTITY_PHI` (Part 6, §10 of the KAIROS doc) and is the single hardest piece
of the analyzer — flagged explicitly as its own milestone (§9, Milestone 4).

A branch that ends in `return` never reaches the join, so it takes no part
in the merge.

A `while` loop is a join too: the state the loop is entered with meets the
state at the end of its body. PHASE requires them to agree on every entity
that existed before the loop — the body must leave each one exactly as it
found it. That makes the body's effect a loop invariant, so checking it
once covers any number of iterations, including zero. A body that has to
rotate buffers between roles each iteration restores the invariant with
`swap` (§3.13).

### 4.4 What the type system guarantees

For every program `phase check` accepts, within a function body:

- **Domains.** No use, borrow, field read, or `destroy` of a `@DMA`/
  `@DEVICE`-owned entity by the host without a `sync`; every `move`/`sync`
  follows §2.4.
- **Borrows.** Many readers or one writer, never both. While a write
  borrow is live the owner is reachable only through it; while only read
  borrows are live the owner may be read (a `read` parameter, a field
  read) but not written, moved, synced, destroyed, or swapped. A released
  handle is dead. Handles cannot be moved, synced, destroyed, swapped, or
  re-borrowed.
- **Read-only access.** A read borrow or a `read` parameter is never
  passed to a `write` parameter, and a `read` parameter is never moved,
  synced, destroyed, or write-borrowed (§3.12).
- **Aliasing.** One storage location appears at most once among a call's
  arguments, unless every occurrence is read-only (a `read` parameter or a
  read borrow). Entities cannot be copied with `let`, and names cannot be
  redeclared, so one location never has two independently tracked names;
  `swap` and `rotate` move storage between names together with its
  tracked state. The parts of a split buffer are disjoint slices of it,
  and the buffer itself is unusable until they are joined (§3.14), so a
  whole-buffer view never coexists with a part.
- **Lifetimes.** No access after `destroy`. At every `return` (explicit or
  falling off the end): no local is still `@DMA`/`@DEVICE`-owned, no borrow
  is live, and every parameter is in the domain its signature promises —
  the one it arrived in, or the `@TO` of a declared change (§3.11).
  Likewise, an entity declared inside an `if`/`while` block is not
  hardware-owned when the block ends. Parameters cannot be destroyed (the
  caller owns them).
- **Types.** Every buffer argument has exactly the parameter's element
  type and length, or the parameter's length is `_` (§3.3) — so a callee,
  and every kernel it forwards to, is told the buffer's real length.
- **Typestate.** A value's state is established only by a function's
  declared return state, or — for a newly constructed value — the
  machine's initial state; a type annotation is checked against that and
  never trusted on its own. Typestate values are linear: passing one to a
  state-requiring parameter, or returning it, consumes it.
- **Control flow.** At an `if` join every incoming path agrees on each
  pre-existing entity's domain, typestate, destroyed-ness, and borrows
  (paths that `return` do not reach the join; a value consumed on one path
  only is unusable after it). A `while` body leaves every pre-existing
  entity exactly as it found it, so the single-pass check is a loop
  invariant and holds for any iteration count.
- **MMIO.** Every `volatile_read`/`volatile_write` targets a declared
  device register (or a simulated `@MMIO` one) and is lowered to a
  `volatile` access, so the C compiler cannot eliminate or reorder it. A
  device register is only read if readable and written if writable, a
  field write only on a register that can also be read, and a literal
  only if it fits (§3.7); the register map itself is laid out exactly as
  declared, which the C compiler checks.
- **Interrupts.** No field write in thread code can lose a write made by
  an interrupt handler (it is inside `critical`, or no handler writes that
  register); no `wait` is reachable from a handler or inside `critical`;
  every `wait`ed event is signalled somewhere; interrupts are re-enabled
  at the end of every `critical` block. A buffer handed back by `wait E
  for b` was the hardware's, and within a function `E` always hands back
  the same buffer (§3.16).

**Trust boundary.** `extern fn` bodies are not analyzed (§3.9): PHASE
trusts that a kernel does only what its signature says — touches only the
buffers it is passed, does not retain them past the call, writes only
parameters not declared `read` (this part is enforced by the C compiler
through `const`, §3.12), and really performs the state transition and
domain change its signature claims. A buggy or
malicious extern can violate any guarantee above. Likewise the analysis is
per-function and assumes the hardware honors the ownership model (e.g.
that a transfer is complete when `sync` returns).

---

<a name="part-5-runtime--simulated-hardware"></a>
## Part 5: Runtime & Simulated Hardware

### 5.1 Why simulated hardware

`phase build` targets host execution by default so the whole project is
testable and demoable without real RF/embedded hardware. Every "device" is a small C
runtime component with realistic *timing and ownership* behavior, not real
signal behavior.

### 5.2 Simulated components

- `runtime/phase_runtime.c` — `sim_dma_wait` / `sim_device_wait`, the two
  functions every `sync` lowers to. Everything shares one address space;
  what is simulated is the *timing*: capture/playback externs record a
  transfer delay, and the wait functions block until it has elapsed, so
  `sync` has observable sequencing behavior.
- `runtime/<program>_extern.c` — hand-written bodies for a program's
  `extern fn`s, e.g. a capture stand-in that fills a buffer with a
  synthetic signal (sine + deterministic noise) so the demo pipeline has
  something real to filter.
- `@MMIO` registers — generated as `volatile` globals; demo externs that
  play the hardware's role read and write the same globals, which is how
  `examples/mmio_poll.phase` observes a register changing under it.

### 5.3 Real hardware

The runtime interface (`sim_dma_wait`, `sim_device_wait`, and `volatile`
register globals) is written so a real backend (Linux DMA-API, an SDR
driver, a microcontroller HAL) can implement it without changing the
compiler. `runtime/hw/` is one such backend: an ARM Cortex-M3 (MPS2 AN385)
startup file, linker script, register map, and wait functions that poll a
real peripheral register, validated under QEMU (see M10 in §9).

`phase build --target=mps2-an385` (v2.0) builds for it: the generated C is
compiled with `arm-none-eabi-gcc` (or `$PHASE_ARM_CC`) for the Cortex-M3
together with `runtime/hw/` — startup and vector table, linker script,
the hardware wait functions, and `memset`/`memcpy`, since a bare-metal
build has no libc — into `build/<stem>.elf`, with every `device` at its
declared address. A program's externs come from `runtime/hw/<stem>_extern.c`
if it exists, else `runtime/<stem>_extern.c`; they must be freestanding.
`--run` runs the result on `qemu-system-arm -M mps2-an385` (or `$PHASE_QEMU`)
with semihosting on, so the program exits cleanly when `main` returns.
`--checked` and `--stub-externs` are host-only: the checker and the stubs
print and exit through libc. `examples/uart_hello.phase`, a UART driver
written in PHASE, prints "Hello from PHASE" this way, and
`examples/timer_ticks.phase` takes five real timer interrupts (§3.16).
`examples/devices/mps2_an385.phase` lists the board's devices and the IRQ
numbers checked on QEMU's model: Uart0 transmit 1, Timer0 8, Timer1 9.

---

<a name="part-6-compiler-architecture"></a>
## Part 6: Compiler Architecture

### 6.1 Pipeline

```
Source -> Lexer -> Parser -> AST
       -> Analyzer (names, domains, ownership, typestate, lifetimes, merges)
       -> PIR Generator -> C Codegen -> cc -> Binary
```

### 6.2 Why C, not LLVM, for v1

Compiling to readable C keeps 100% of engineering time on the analyzer (the
actually novel part). LLVM backend is explicitly deferred (§10).

### 6.3 Crate layout (Rust workspace)

```
phase/
  crates/
    ast/            -- AST type definitions (shared)
    lexer/          -- tokens
    parser/         -- AST construction
    analysis/       -- name, domain, ownership, typestate, lifetime and
                       control-flow analyzers (one pass over each fn body)
    pir/            -- Phase IR data structures + builder
    codegen_c/      -- PIR -> C source
    driver/         -- the `phase` CLI, including the language server
                       (`phase lsp`, §6.8)
    fuzz/           -- phase-fuzz: differential fuzzer (§6.7)
  runtime/
    phase_runtime.c / .h   -- sim_dma_wait / sim_device_wait (host)
    phase_check.c / .h     -- runtime ownership checker for --checked (§6.7)
    <program>_extern.c     -- hand-written bodies for a program's extern fns
    hw/                    -- ARM Cortex-M3 backend (§5.3)
  editors/                 -- TextMate grammar, VS Code extension, Vim
                              syntax (§6.8)
  examples/
    *.phase                -- programs that pass analysis
    bug_gallery/*.phase    -- one rejected program per bug class (§8)
```

Name resolution and type checking happen inside `analysis` rather than a
separate `sema` crate: the language has no overloading and its only
generic is a buffer's length (`buffer<T, _>`, §3.3), so a separate pass
would only duplicate the symbol tables the analyzer already builds.

### 6.4 CLI

```
phase [options] check <file>   -- run all analysis passes, report errors, no codegen
phase [options] build <file>   -- full pipeline, emits a native binary
phase [options] --dump-tokens <file>
phase [options] --dump-ast <file>
phase [options] --dump-pir <file>
phase lsp                      -- language server on stdin/stdout (§6.8)
phase --version | --help

options:
  --error-format=human|short   (default: human, see §6.5)
  --color=auto|always|never    (default: auto -- respects NO_COLOR, and
                                only colors a terminal)
  --checked                    (build) runtime-checked build, see §6.7;
                                writes <stem>.checked.gen.c and <stem>-checked
  --stub-externs               (build) generate placeholder extern bodies
                                instead of requiring runtime/<stem>_extern.c
  --target=host|mps2-an385     (build) host (default), or a Cortex-M3 .elf
                                for the MPS2 AN385 board (§5.3)
  --run                        (build) run the result: the host binary, or
                                the .elf on qemu-system-arm
```

`build` locates the runtime by walking up from the source file to the
nearest `runtime/` directory (override: `PHASE_RUNTIME_DIR`) and writes
into a `build/` directory beside it. Exit status is 0 on success and 1 on
any error.

### 6.5 Diagnostics

Every token carries a source span (start and end, line and column). The
parser attaches spans to every statement, function name, and parameter,
and the analyzer reports each violation with:

- a **primary location** — the statement that breaks the rule (an error
  inside a branch keeps the inner statement; a disagreement at a join
  points at the `if`/`while`; falling off the end of a function points at
  its closing `}`), narrowed to the entity's name where it appears there;
- **notes** at the locations that explain it — where the entity was
  declared, moved, synced, borrowed, released, destroyed, or consumed;
  where each branch left it; where a typestate value got its state; which
  call changed its domain.

`human` format renders each as a source snippet:

```
error: entity 'frame' is in domain @DMA but 'report' expects @RAM for this argument
  --> examples/bug_gallery/read_while_capture_in_flight.phase:19:12
   |
19 |     report(frame);
   |            ^^^^^
note: 'frame' is moved to @DMA by 'start_capture' here
  --> examples/bug_gallery/read_while_capture_in_flight.phase:16:5
   |
16 |     start_capture(frame);
   |     ---------------------
```

`short` format prints one `file:line:col: error|warning|note: message`
line per diagnostic and note, for editors and scripts. Parse errors use
the same renderer. In a program of several files (§3.15), each location
— the error and each note — is shown in the file it is in; a missing
import is reported at the `import` that names it.

**Warnings** (v1.2) use the same format with a `warning` label. They never
reject a program or change the exit status; `check` reports their count on
its success line. No rule warns in 2.0: the only warning, read-only data
passed to a parameter with no access mode, is an error now (§3.12).

### 6.8 Editor support

`phase lsp` is a Language Server Protocol server on stdin/stdout, built
into the compiler. On every open and change of a document it runs the
same parser and analysis as `phase check` and publishes the result as LSP
diagnostics: the same underlined range (narrowed to the entity a message
is about), errors and warnings with their severities, and every note as
related information. Imports are read from open documents first (so unsaved edits count),
then from disk; a problem inside an imported file is shown on the
`import` line that brought it in, prefixed with its file and line and
linked to the real location. It answers `textDocument/documentSymbol` with the
file's functions and devices (with their registers), follows the
`shutdown`/`exit` protocol, and converts columns to UTF-16 as the
protocol requires.

**Hover and go-to-definition (v2.3).** Hovering a buffer or another
local shows what the analyzer knows about it at that line — its type, its
domain (and so who owns it), whether it is borrowed, split, a part of a
split buffer, destroyed or consumed — followed by the statements before
that line in the function that changed it (`- line 12: start(b); → in
@DMA (the DMA engine has it)`). The state shown is the one the statement
under the cursor starts in; on a line that declares the name, the one it
ends in. A statement checked more than once (in a loop, or on paths that
disagree in a rejected program) shows each state it was seen in.
Hovering a function, extern, device, register, field or event shows its
declaration; an event also lists the functions that signal and wait for
it, a device its registers. `textDocument/definition` jumps to the
declaration of the same names — a local's `let`/`buffer`/`split`, a
parameter in its function's header, a register field in another file of
the program. Other requests get `MethodNotFound`.

`editors/` has a TextMate grammar (in a VS Code extension that also
starts `phase lsp`), a Vim syntax file, and setup notes for Neovim,
Helix and Emacs. CI tokenizes every example with the grammar and checks
the scopes it assigns.

### 6.6 Transition table representation

v1 keeps the domain-transition table (§2.4) as a hardcoded Rust `match` /
static table inside `analysis`, not a plugin/config file. This is a
deliberate scope cut versus KAIROS's `.hardware` file idea — making it
data-driven is easy future work once the core analyzer is proven correct.

### 6.7 Runtime-checked builds and differential fuzzing

The static analysis is the guarantee; these two tools are the evidence
that it holds.

**`phase build --checked`** emits C that verifies the ownership rules
while the program runs. `runtime/phase_check.c` keeps a shadow state for
every buffer storage — its size in bytes, its domain, whether it was
destroyed or is split, and the borrows taken of it (with the call frame
each was taken in) — keyed by the storage's address, so it follows `swap`,
`rotate` and borrows without extra bookkeeping. The generated code checks
it at every buffer creation, `move`, `sync`, borrow, use and release of a
borrow, `destroy`, `swap`, `rotate`, `split`, `join`, call argument
(domain, access mode, borrows, aliasing, and that the buffer really has
the bytes the callee is told it has — its declared length, or the length
passed for a `_` parameter), extern domain change, parameter exit, and
scope exit (every block end and every `return`). A split buffer's parts
are checked to be its slices, and `join` checks that each part still
holds its own slice and that all are in one domain.

Registers and interrupts (v2.3): every register access is checked
against the register's declared access, and every written value — a
literal, or one computed at run time, which the analyzer doesn't see —
against the width of the register or field. A `wait` inside a handler or
inside `critical` fails; so does a `wait` 1,000 rounds of handlers don't
satisfy, a function or `main` returning inside `critical`, and a `wait E
for b` naming a different buffer than an earlier one for `E` in the same
function (and `b` itself is checked like a `sync`). Lost writes are found
by making them happen: a field write in thread code outside `critical`
reads the register, runs every interrupt handler, and writes the result —
and a handler writing that same register in between is reported as the
lost write it would be.

Typestate values are tracked by the address of the variable holding
them: their state comes from the callee's *declared* return state (or,
for a value built directly, must be the machine's initial state) — never
from an annotation — and every state-requiring argument and `return`
checks the state, checks that the value was not already consumed, and
consumes it unless the parameter is `read`. A violation prints `PHASE
runtime check failed: …` and exits with status 70.

The checker is deliberately independent of the analyzer: it never reasons
about branches, joins, or loop invariants — where static-analysis bugs
live — it just watches one concrete execution. Borrow rules apply per
call frame: a borrow a caller holds is what grants a callee its access to
the parameter. Struct entities other than typestate values are not
tracked at run time.

**`phase build --stub-externs`** generates placeholder bodies for a
program's `extern fn`s. Scalar results come from a PRNG seeded by
`PHASE_STUB_SEED`, so a program's paths can be explored without writing
any C.

**`phase-fuzz`** (crate `crates/fuzz`) combines the two. It generates
random, well-formed programs — buffers of three types (`u8` × 8, `u8` ×
16, `f32` × 8) in every domain, borrows, calls to externs and helpers
with fixed and `_` lengths, access modes and domain changes, `swap`,
`rotate`, `split` (into 2 or 4, nested) and `join`, a typestate machine
whose values are created, advanced, inspected and consumed, nested
`if`/`while`, early returns — mostly consistent with a model of each
buffer's and value's state, partly not. Deviations for the rarer rules
are targeted rather than left to chance: a forged state annotation is
then used as the claim says, a consumed or outer-loop value is reused, a
split buffer is joined right after one part is handed to the engine or
two parts are swapped out of place. Every program `phase
check` accepts *without warnings* is built with `--checked` and
`--stub-externs` and run with several stub seeds. An accepted program that
trips a runtime check is a soundness bug in the analyzer. Every program
the fuzzer has flagged is kept in `crates/fuzz/regressions/` and replayed
by the test suite; CI fuzzes a fresh seed range on every run.

Its first runs found four bugs, all fixed in v1.3: a buffer declared in a
block leaving scope while DMA-owned, `swap` across blocks leaving a
dangling name, an early `return` in `main` with an undefined exit status,
and a false alarm in the runtime checker itself. In v1.4 it found two
more false alarms in the runtime checker, both in `split` (a buffer, its
first part and that part's first part share one address; a buffer split
under a name `rotate` had moved), before the release.

**Validating the fuzzer.** Silence is only evidence if the fuzzer would
have spoken. With the block-end check (the first fix above) deliberately
removed from the analyzer, the fuzzer reported 8 violations in the first
1,000 programs of a seed range that the real analyzer passes with none.
With the call-site domain check disabled — the most basic rule — it
caught that too, more often at a higher `--chaos` setting (3 findings in
300 programs at `--chaos 0.3`, versus 1 at the default). After the fixes,
6,500 freshly generated programs (2,973 accepted and run with several
stub seeds each, including 1,500 at `--chaos 0.3`) produced no
violations.

Each rule family added to the generator in v1.4 was validated the same
way, one analyzer check removed at a time, against seed ranges the real
analyzer passes cleanly:

| Check removed from the analyzer | Programs | Violations found |
|---|---|---|
| buffer element type / length at call sites | 500 | 1 (a size violation) |
| typestate consumption | 500 | 3 (use after consume) |
| typestate annotations checked against the producer | 500 | 1 (a forged state) |
| a split buffer unusable while split | 1,000 | 1 |
| `join`: parts in one domain | 1,000 | 5 |
| `join`: each part holds its own slice | 1,000 | 5 |

The first two typestate mutants were *not* caught until the generator was
rebalanced (typestate values were created 0.7 times per program, and a
consumed value reused in 1 program in 400; now 2.8 and 36 in 400), and
the two `join` mutants not until the targeted deviations above existed —
which is the point of checking. Whole-buffer use while split is caught
least often: the analyzer's other rules usually reject such a program for
a second reason. For the release, 6,000 freshly generated programs from the 1.4
generator (2,619 accepted and run with several stub seeds each, including
1,500 at `--chaos 0.3`) produced no violations. After the 2.0
changes, another 4,000 (1,710 accepted and run, including 1,000 at
`--chaos 0.3`) produced none either; after 2.1's, another 4,000
(1,740 accepted and run), and after 2.2's, another 4,000 (1,816).

**Registers and interrupts (v2.3).** The generator now declares a device
— four registers: read-write with fields, read-only, write-only, and a
narrow read-write one with fields — and three events, and most programs
get one or two interrupt handlers that write registers and signal
events, unconditionally, every time they run. The code they interrupt
reads and writes registers, waits for events, has buffers handed over
with `wait E for b`, signals, and runs blocks in `critical`. Signals in
thread code are only generated for events a handler signals too: a
`signal` on a path not taken would hang a `wait` the analyzer accepted,
which it doesn't promise against. A handler deviates as a whole (it
waits, or writes a register wrongly) or not at all, and the rules about
*where* something happens — a field write outside `critical`, a `wait`
or `return` inside it, an event handing back a second buffer, a buffer
handed back that the hardware doesn't have — deviate at a higher rate.
`main` ends by calling every helper once with fresh buffers, so helper
bodies run at least once. Mutation results, one analyzer check removed at
a time, against the first 1,500 programs of a seed range the real
analyzer passes cleanly:

| Check removed from the analyzer | Violations found |
|---|---|
| field write in thread code to a register a handler writes | 9 (lost writes) |
| `wait` in handler code | 9 |
| `wait` inside `critical` | 4 |
| `return` inside `critical` | 2 |
| `wait` on an event nothing signals | 16 |
| an event hands back one buffer per function | 3 |
| `wait E for b`: `b` must be the hardware's | 11 |
| reading a write-only register | 8 |
| writing a read-only register | 5 |
| a literal fits its register or field | 39 |

Three of these were *not* caught by the first version of the generator:
a `return` inside `critical` always left statements after the block (so
every such program was rejected as unreachable code), and the two
`wait E for b` mutants were reached only in helpers nothing called.
Ending generation at that `return`, the targeted deviations, and calling
every helper from `main` fixed that. With the real analyzer, 5,000 freshly
generated programs from the 2.3 generator (1,925 accepted and run with
several stub seeds each, including 1,000 at `--chaos 0.3`) produced no
violations.

**Scope of the evidence.** Struct entities other than typestate values
are not tracked at run time; the generator covers one typestate machine,
three buffer types and one device, with handlers that call nothing. Those
parts are covered by the static tests and the bug gallery, not by
fuzzing.

---

<a name="part-7-pir--phase-intermediate-representation"></a>
## Part 7: PIR — Phase Intermediate Representation

### 7.1 Purpose

PIR is the internal representation between the analyzer and C codegen. It
preserves everything C cannot express natively: entity identity, domain,
ownership state, and history.

### 7.2 Core instructions

```
ENTITY_CREATE   id, type, domain
ENTITY_MOVE     id, from_domain, to_domain
ENTITY_SYNC     id
ENTITY_BORROW   id, mode(R|W)
ENTITY_RELEASE  id
ENTITY_TRANSFORM id, from_state, to_state
ENTITY_DESTROY  id
ENTITY_CALL     fn, args[]           -- extern fn calls (DSP kernels)
ENTITY_ROTATE   ids[]                -- swap is the two-name case
ENTITY_SPLIT    id, parts[]
ENTITY_JOIN     id, parts[]
ENTITY_PHI      id, [ (block, domain, state) ... ]   -- merge point
```

### 7.3 Verification (must hold before codegen)

1. Every entity has exactly one domain at every program point.
2. Every `ENTITY_MOVE`/`ENTITY_SYNC` corresponds to a legal transition.
3. No `ENTITY_*` access occurs after that entity's `ENTITY_DESTROY`.
4. No conflicting borrows are live simultaneously.
5. Every `ENTITY_PHI` has agreeing domain/state across all incoming edges.

### 7.4 Lowering to C

```
@RAM     -> plain pointer
@DMA     -> pointer + call to sim_dma_wait() at each ENTITY_SYNC
@MMIO    -> volatile pointer, direct volatile read/write, never cached
@DEVICE  -> pointer + call to the relevant sim_* function
```

### 7.5 Debug output

`--dump-pir` prints the lowered PIR for a program that passed analysis.
*Why* a program was rejected is shown by the diagnostics themselves
(§6.5), whose notes trace the entity's history up to the violation.

---

<a name="part-8-bug-gallery--what-the-compiler-must-catch"></a>
## Part 8: Bug Gallery — What the Compiler Must Catch

Each entry below is one deliberately-broken example program plus the exact
compile error PHASE must produce. This doubles as the acceptance-test suite
for the analyzer and as the security narrative for the portfolio write-up.

| # | Program pattern | Real-world analogue | Compiler must report |
|---|---|---|---|
| 1 | CPU reads a `@DMA` buffer with no prior `sync` | Reading a transfer buffer before DMA completion | "entity `X` is in domain @DMA but `f` expects @RAM" |
| 2 | `move` or `destroy` while a `borrow` is live | Freeing a buffer another part of the code still references | "entity `X` has a live borrow; cannot move/destroy" |
| 3 | Two concurrent `write` borrows | Data race on a shared buffer | "entity `X` already has an exclusive write borrow" |
| 4 | Access after `destroy` | Use-after-free | "entity `X` was destroyed; cannot use as argument to `f`" |
| 5 | Calling a function requiring `Packet<Decoded>` with a `Packet<Received>` | Parsing/validating a packet in the wrong order (TOCTOU) | "expected state Decoded, found Received" |
| 6 | Branch merges with mismatched domain/state on the two paths | Missing sync on an error path | "entity `X` disagrees on domain/state across incoming branches" |
| 7 | Reordering/eliding a `volatile_read`/`volatile_write` sequence | Compiler "optimizing away" a hardware side effect | (verified negatively — codegen test asserts both accesses appear in emitted C, in order) |
| 8 | Annotating a value with a state it was never given | Asserting verification instead of performing it | "`p` is declared as Packet<Validated> but `f` returns Packet<Received>" |
| 9 | Reusing a typestate value after passing it on | Double-processing / replay | "value `p` was already consumed (passed to `f`)" |
| 10 | `destroy` while `@DMA`/`@DEVICE`-owned | Freeing memory a device is still writing | "entity `X` is still owned by @DMA; sync(X) before destroying it" |
| 11 | Returning while a local is `@DMA`/`@DEVICE`-owned | DMA into an out-of-scope buffer | "entity `X` is still owned by @DMA when `f` returns" |
| 12 | Using the owner while it is borrowed | Bypassing exclusive access | "entity `X` is borrowed by `v`; it cannot be used directly" |
| 13 | Using a borrow after `release` | Dangling reference | "borrow `v` of `X` was already released" |
| 14 | Same buffer passed twice to one call | In-place call of a non-aliasing kernel | "entity `X` is passed to `f` more than once" |
| 15 | Borrowing a `@DMA`-owned buffer | CPU access before transfer completion | "entity `X` is owned by @DMA; sync(X) before borrowing it" |
| 16 | Redeclaring a name over a live entity | Losing track of an in-flight buffer | "`X` is already declared in fn `f`" |
| 17 | A helper declared `@DMA -> @RAM` that never syncs | A "wait" function that doesn't wait | "parameter `b` must be in @RAM when `f` returns (its signature says @DMA -> @RAM)" |
| 18 | A signature claiming an illegal domain change | A trusted kernel promising an impossible hand-off | "no legal transition from @DMA to @DEVICE in the signature of `f`" |
| 19 | Using a buffer between start-transfer and wait calls | Use-before-sync spread across functions | "entity `X` is in domain @DMA but `f` expects @RAM" (note: moved to @DMA by `start`) |
| 20 | Passing a read borrow to a `write` parameter | Writing through a read-only view | "`f` may write its parameter `x`, but `v` is read-only" |
| 21 | A `read` parameter handed to a writer | A helper breaking its own read-only promise | "`f` may write its parameter `x`, but `p` is read-only" |
| 22 | Restarting the capture into the buffer being processed | Torn frame in a ping-pong pipeline | "entity `ping` is in domain @DMA but `process` expects @RAM" |
| 23 | Ping-pong loop without trading the buffers | Waiting on the idle buffer, reprogramming a running transfer | "entity `ping` is @RAM at the end of the `while` body but @DMA when the loop is entered" |
| 24 | `swap` while one buffer is borrowed | A view left pointing at the wrong buffer | "entity `X` has a live borrow; cannot swap" |
| 25 | A buffer declared in a block, left DMA-owned at its end | DMA into reused stack memory | "entity `X` is still owned by @DMA when its block ends" |
| 26 | `swap` with a buffer from an inner block | A name left pointing at dead storage | "cannot swap `X` with `Y`: `X` is declared in an inner block" |
| 27 | A shorter buffer passed where a longer one is required | Buffer overflow in the kernel | "`f` expects buffer<f32, 256> for `p`, but `x` is buffer<f32, 64>" |
| 28 | A buffer of the wrong element type | Type confusion, reading past the end | "`f` expects buffer<f32, 64> for `p`, but `x` is buffer<u8, 64>" |
| 29 | A run-time-length buffer passed where a fixed length is required | Unchecked length assumption | "`f` expects buffer<f32, 128> for `p`, but `b` is buffer<f32, _>" |
| 30 | Processing the half a circular DMA engine is still filling | Off-by-one-half in a circular ADC driver: a torn block | "entity `hi` is in domain @DMA but `process` expects @RAM" |
| 31 | Using the whole buffer while it is split | A whole-buffer view over a half still being written | "`ring` is split into lo, hi; join them back first" |
| 32 | Joining the halves while one is still in flight | Stopping the stream for one half only | "cannot join `lo` (@RAM) and `hi` (@DMA) into `ring`: its parts must all be in the same domain" |
| 33 | Rotating a triple-buffer ring the wrong way | Waiting on an idle buffer, restarting one in flight | "entity `b` is @RAM at the end of the `while` body but @DMA when the loop is entered" |
| 34 | Clearing a flag by writing a read-only status register | A write the hardware ignores; a stale "data ready" | "`Adc.STATUS` is read-only; it cannot be written" |
| 35 | Reading a write-only register | Trusting whatever a command register reads back as | "`Adc.CLEAR` is write-only; it cannot be read" |
| 36 | Setting one field of a write-only register | `CMD \|= START;` clobbering every other bit | "cannot write field `Dma.CMD.START`: `Dma.CMD` is write-only" |
| 37 | A value too wide for its field | Spilling into the neighbouring field (here, the enable bit) | "16 does not fit in `Timer.CTRL.PRESCALE` (4 bits, at most 15)" |
| 38 | Two registers at one offset | A copy-pasted register map | "register `Timer.VALUE` (bytes 0x4..0x8) overlaps `Timer.CTRL`" |
| 39 | Read-only data passed to a parameter with no access mode | A legacy API that may write what it is given | "`checksum` doesn't declare whether it writes its parameter `x`, but `view` is read-only" |
| 40 | Two devices at one address | A copy-pasted base address in a board file | "devices `DebugUart` (0x40005000..0x40005014) and `ModemUart` (0x40005000..0x40005014) overlap" |
| 41 | A field write an interrupt handler can split | A lost register update: a one-shot timer that keeps firing | "writing field `Timer0.CTRL.ENABLE` reads `Timer0.CTRL` … interrupt handler `on_timer0` (IRQ 8) also writes `Timer0.CTRL`" |
| 42 | Waiting inside an interrupt handler | A handler that sleeps forever, blocking its priority level | "`wait tx_done` in interrupt handler code would never return" |
| 43 | An event handing back the wrong half | Processing the half of a circular buffer the DMA engine is still writing | "`wait half_done for hi`, but `half_done` hands over 'lo' earlier in this function" |
| 44 | Waiting for an event nothing signals | A board that hangs because a refactor dropped a handler's `signal` | "`wait rx_ready` never returns: nothing signals 'rx_ready'" |

Every file in `examples/bug_gallery/` carries its expected error in an
`// Expected: "..."` header, and a test asserts the analyzer produces it.

---

<a name="part-9-implementation-roadmap--milestones"></a>
## Part 9: Implementation Roadmap & Milestones

Each milestone below produces something runnable/testable — no milestone
depends on the whole system existing first.

**M1 — Lexer + Parser + AST**
Grammar for entities, domains, `move`/`sync`/`borrow`, functions. Output:
`phase --dump-ast` works on `examples/radio_pipeline.phase` (straight-line,
no branches yet).

**M2 — Type checker + fixed transition table, straight-line programs only**
No control flow. Domain checker + ownership checker for sequential code.
Output: `phase check` correctly accepts the happy-path example and rejects
bug-gallery items #1–#4.

**M3 — State analyzer (typestate)**
Adds `state`/typestate checking. Output: rejects bug-gallery item #5.

**M4 — Control flow + ENTITY_PHI / merge checking**
`if`/`while` support in AST, parser, and analyzer. This is the hardest
milestone — budget the most time here. Output: rejects bug-gallery item #6.

**M5 — PIR generation + C codegen + sim runtime**
Full pipeline to a running binary against `sim_dma`/`sim_adc`/`sim_dac`.
Output: `phase build` on the happy-path pipeline produces a binary that
actually runs the simulated capture → filter → playback sequence.

**M6 — MMIO + volatile semantics**
Output: rejects/accepts appropriately, and a codegen test proves volatile
ordering is preserved (bug-gallery item #7).

**M7 — Demo + write-up**
Full `radio_pipeline.phase` demo (synthetic signal in, FIR filter via
`extern fn`, synthetic signal out) plus the full bug gallery with real
compiler error output captured, plus a short doc mapping each caught bug
class to a real-world CVE/incident category.

**M8 — Real control-flow codegen**
Closes M5's documented scope cut: `PirInst::If`/`PirInst::While` lower
`if`/`while` to real, correctly-indented, arbitrarily-nested C `if`/
`while` statements (`phase_pir`/`phase_codegen_c`), instead of
`phase build` rejecting any body that uses them. Output:
`examples/branching_pipeline.phase` builds with `cc` and a driver
end-to-end test runs *both* branches of a real conditional, selected at
runtime (`PHASE_DEMO_USE_DMA`) -- not just that the M4 analyzer accepts
the branching source. Condition expressions are deliberately scoped to
`PirExpr` (identifiers, literals, binary comparisons/arithmetic) rather
than the full `Expr` grammar; see the Honest Scope note on assignment
below for why a loop can't yet mutate its own condition.

**M9 — Assignment + richer expressions**
The language had no assignment statement -- a `let`-bound local never
changed value after it was declared, so a `while` loop's condition could
never change between iterations. M9 adds `name = expr;`
(`Stmt::Assign`/`PirInst::Assign`), scoped to plain scalar locals only
(domain-tracked entities/buffers keep using `move`/`sync`/`borrow`/
`destroy`, not assignment -- reassigning through them would bypass those
checks). Output: `examples/counting_loop.phase` builds and a driver
end-to-end test runs a real `while n < 5 { .. n = n + 1; }` loop and
confirms it prints exactly `n=0` through `n=4`, not zero times, not
forever. Condition/RHS expressions are still the same `PirExpr` subset
from M8 (identifiers, literals, binary comparisons/arithmetic) -- no
calls yet, so a condition can't directly poll `volatile_read(...)`.

**M10 — Real hardware target**
Replaces `runtime/phase_runtime.c`'s two simulated wait functions
(`sim_dma_wait`/`sim_device_wait`) with `runtime/hw/phase_runtime_hw.c`,
a real busy-poll of a real, documented hardware register -- on a real
ARM Cortex-M3 target (the MPS2 AN385 board), not the host's simulated
address space. By design this only touches the runtime layer: the
compiler, `phase_codegen_c`'s output, and the PHASE source
(`examples/hw_uart_echo.phase`) are all byte-for-byte the same as every
other example. Output: `./scripts/build_hw_demo.sh` cross-compiles with
`arm-none-eabi-gcc` (`-mcpu=cortex-m3 -mthumb -ffreestanding -nostdlib`)
against a real linker script/vector table (`runtime/hw/`) and runs the
resulting ARM binary under `qemu-system-arm -M mps2-an385` -- QEMU's
model of that real board's peripherals and memory map, a standard way to
validate firmware without physical silicon on the bench. (Since v2.0 this
is `phase build --target=mps2-an385 --run`; the script just runs that.)
Verified three ways: (1) a driver end-to-end test cross-compiles and
actually runs the binary under QEMU, asserting on its real UART0 output;
(2) `arm-none-eabi-objdump` disassembly confirms `sim_device_wait` really
compiles to `mov r2, #0x40004000` / `ldr`/`tst`/`bne` -- an actual
register poll, not a stand-in; (3) the CMSDK UART register map
(`runtime/hw/mps2an385.h`) is cross-checked against QEMU's own board
model source, not guessed. Honest limit: the MPS2 AN385 QEMU model has
no real DMA controller, so both wait functions poll the one real
peripheral this board exposes (the UART) -- a target with an actual DMA
controller would poll that controller's own completion register the
same way, same pattern, different register.

**M11 — `volatile_read` in conditions/assignment RHS (real MMIO polling loops)**
Closes the gap directly: `PirExpr` gains a `VolatileRead` variant, so
`volatile_read(REG @MMIO)` can now appear inside an `if`/`while`
condition or an assignment right-hand side -- e.g.
`while volatile_read(READY @MMIO) == 0 { .. }`, a real hardware polling
loop expressed entirely in PHASE source instead of requiring the poll
itself to be hand-written in the runtime. Deliberately restricted to this
one builtin, not general `extern fn` calls in a condition -- see the
`PirExpr::VolatileRead` doc comment for why. Output:
`examples/mmio_poll.phase` builds and runs, polling a simulated-hardware
register across exactly 3 iterations before terminating, with the loop
condition itself 100% compiler-generated code; a companion driver test
recompiles the generated C at `-O2` and inspects the assembly to confirm
the register read genuinely survives optimization (two separate reads,
not one hoisted out of the loop) rather than trusting `volatile`
by convention alone.

Building this milestone surfaced and fixed two real latent bugs from
earlier milestones, both now covered by regression tests: (1) PIR's
`collect_mmio_registers` only scanned top-level instructions, never
recursing into `if`/`while` bodies -- a gap since M8 that nothing had
exercised until a register was referenced only from inside a loop
condition; (2) the M2/M6 analyzer's `check_expr` never recursed into
`Expr::Binary`, so `volatile_read(...) == 0` (a comparison, not a bare
call) never actually got its domain/arity validation run -- confirmed via
`docs/bug_gallery_transcript.md` regeneration that this fix doesn't
change any existing accept/reject outcome.

**v1.0 — Soundness hardening**
An audit of the M1–M11 analyzer found accepted programs that violated the
very rules the bug gallery documents (a typestate forged by annotation,
reuse of consumed values, `destroy` of a DMA-owned buffer, hardware-owned
buffers escaping a function, owner use during a write borrow, use after
`release`, aliased arguments, borrowing unsynced DMA, shadowing, and
unchecked names in conditions). v1.0 closes each one (§4.4 states the
resulting guarantees), adds bug-gallery entries #8–#16, and pins each
gallery file to its documented error in the test suite. It also makes
`phase build` independent of the working directory and adds CI.

**v1.1 — Diagnostics and domain changes in signatures**
Errors now carry source locations: spans on every token, statement,
function name, and parameter; analyzer errors with a primary location and
notes that trace the cause; rustc-style rendering with a `short` format for
tools (§6.5). And a parameter may declare `@FROM -> @TO` (§3.11), so
starting and finishing a transfer can live in separate functions with CPU
work in between, checked across both calls. Output:
`examples/overlapped_capture.phase` builds and runs with a window computed
while the capture is in flight; bug-gallery entries #17–#19.

**v1.2 — Access modes and double buffering**
Parameters may be declared `read` or `write` (§3.12): exact aliasing and
borrow rules, read-only data that stays read-only, non-consuming `read`
typestate parameters, and `const` pointers in the generated C. This closed
a hole — a read borrow could be passed to a parameter that writes — and,
to keep 1.1 programs compiling, introduced warnings. `swap a, b;` (§3.13)
makes ping-pong double buffering expressible and checkable as a loop
invariant. Output: `examples/ping_pong.phase` builds and runs, processing
seven frames in order from alternating buffers; bug-gallery #20–#24.

**v1.3 — Evidence: runtime-checked builds and differential fuzzing**
`phase build --checked` verifies the ownership rules on a concrete
execution, independently of the analyzer, and `phase-fuzz` runs thousands
of random programs the analyzer accepts through it (§6.7). It found two
soundness holes in the analyzer (block-local hardware ownership, `swap`
across blocks) and a codegen bug, all fixed. Buffer types are now checked
at every call — closing a real overflow where a shorter buffer could be
passed — and parameters may be length-generic, `buffer<T, _>` (§3.3).
Output: after the fixes, 6,500 fuzzed programs (2,973 accepted and run)
with no violations, and a fuzzer shown to catch reintroduced analyzer bugs
(§6.7); `examples/multi_rate.phase` runs one set of generic helpers over
two buffer sizes; bug-gallery #25–#29.

**v1.4 — Ring buffers, and fuzzing the whole type system**
`rotate a, b, c;` generalizes `swap` to rings of buffers, and `split` /
`join` give the halves of a circular DMA buffer their own ownership while
the whole is off limits (§3.14). The runtime checker now verifies buffer
sizes, typestate, and split/join, and the fuzzer generates three buffer
types with fixed and `_` lengths, typestate values, rotations and nested
splits — so every rule family except MMIO and plain struct entities is
now exercised by differential fuzzing, with each validated by mutation
(§6.7). Output: `examples/triple_buffer.phase` processes eight frames in
order across three rotating buffers with two always in flight, and
`examples/circular_dma.phase` processes eight blocks from alternating
halves of one circular buffer, then uses it whole; bug-gallery #30–#33.

**v2.0 — Hardware as a compiler feature, editor support, and the 2.0 rule**
`device` declarations (§3.7) give a peripheral's register map — address,
registers with access modes, widths, offsets, reset values and bit fields
— and the analyzer checks every access against it: misspelled names,
writes to read-only registers, reads of write-only ones, field writes
that would read a write-only register, values too wide for a field, and
overlapping or misaligned maps. `phase build --target=mps2-an385 --run`
cross-compiles for the Cortex-M3 board and runs on QEMU (§5.3), replacing
the separate build script. `phase lsp` (§6.8) puts the compiler's
diagnostics in the editor. And, as promised in 1.2, read-only data passed
to a parameter with no access mode is an error. Output:
`examples/uart_hello.phase` — a UART driver written entirely in PHASE —
prints "Hello from PHASE" from the real UART0 data register on QEMU's
board model; bug-gallery #34–#39.

**v2.1 — Programs across files, device types, and a tutorial**
`import` (§3.15) splits a program across files, with every diagnostic
shown in its own file, in the terminal and in the editor. `device type`
(§3.7) declares a peripheral's layout once for all its copies, and
devices may no longer overlap or be misaligned. Output:
`examples/devices/mps2_an385.phase`, board support for the MPS2 AN385
with each address checked on QEMU's board model, imported by
`examples/uart_hello.phase`; bug-gallery #40; and "PHASE in 15 minutes"
(`docs/tutorial/`), generated by running its programs.

**v2.2 — Interrupts**
`interrupt fn` handlers bound to IRQs, `event`s they signal and `main`
waits for, and `critical` sections (§3.16). The analyzer follows the call
graph from every handler to catch a field write a handler can split and a
`wait` that can never return. Output: `examples/timer_ticks.phase` takes
five real timer interrupts on QEMU's MPS2 AN385 model; bug-gallery
#41–#42; a tutorial step.

**v2.3 — Interrupts hand over buffers; fuzzing hardware code; hover**
`wait E for b` lets the interrupt that signals `E` hand buffer `b` back to
`main`, checked like `sync`, with an event always handing back the same
buffer within a function; a `wait` on an event nothing signals is an
error (§3.16). Checked builds verify register accesses and interrupt
rules at run time, including lost writes (§6.7), and the fuzzer generates
devices, handlers, events, waits, hand-offs and critical sections, each
rule validated by mutation. The language server shows a local's owner and
history on hover and jumps to declarations (§6.8). Output:
`examples/circular_dma.phase` streams eight blocks from its interrupt
handler, against a simulated DMA controller in a host build; bug-gallery
#43–#44.

**M12 — LLVM backend** *(not yet started)*
An alternative to `codegen_c` that lowers PIR straight to LLVM IR instead
of C, avoiding a dependency on an external `cc`. See §10 for why this was
deliberately deferred rather than attempted first.

**Suggested testing approach throughout:** every milestone gets a
`tests/` directory of `.phase` files with an expected-output (`.stdout`/
`.stderr`) golden file, run via `cargo test` -- both accept-cases and the
bug-gallery reject-cases, so regressions in the analyzer are caught
immediately as the language grows.

---

<a name="part-10-non-goals"></a>
## Part 10: Non-Goals

- Not a replacement for C, Rust, or CUDA.
- No LLVM backend (documented future work, M12).
- One hardware target (the MPS2 AN385 board, §5.3). Others need their own
  startup, linker script and wait functions under `runtime/`.
- No interprocedural analysis beyond signatures: each function is checked
  against the signatures of the functions it calls — including any
  declared domain changes (§3.11) — and `extern fn` bodies are trusted
  (§4.4).
- No GPU kernel compilation — accelerators are just another `@DEVICE`.
- No garbage collector; no automatic parallelism; no distributed/NUMA
  domains.
- No plugin/config-file-driven hardware description in v1 — the
  transition table is fixed and hardcoded (§6.6), matching v1 scope.
- Not attempting to be a general-purpose language: `if`/`while`/functions
  exist only to the extent the pipeline-safety demos need them.
