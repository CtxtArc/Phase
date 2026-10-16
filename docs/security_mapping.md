# Bug Gallery → Real-World Security Categories

Every rule PHASE's analyzer enforces exists because a real, named class of
bug keeps recurring in production embedded, driver, and protocol code. This
doc maps each bug-gallery item to its [MITRE CWE](https://cwe.mitre.org/)
category and, where a clean, well-documented real-world instance exists, a
specific CVE or research source. See
[`docs/bug_gallery_transcript.md`](bug_gallery_transcript.md) for the real,
captured compiler output for every item below.

---

## #1 — Using a `@DMA`/`@DEVICE` buffer before `sync`

**`examples/bug_gallery/use_before_sync.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html) —
Concurrent Execution using Shared Resource with Improper Synchronization
("Race Condition"); closely related to
[CWE-367](https://cwe.mitre.org/data/definitions/367.html) (TOCTOU).

**Real-world precedent:** this is a studied, named problem in Linux driver
code, not a hypothetical. A USENIX Security 2021 paper, *Static Detection
of Unsafe DMA Accesses in Device Drivers*, built a static analyzer
specifically to find places where driver code touches a streaming DMA
buffer without the required explicit synchronization call — streaming DMA
buffers require the driver to explicitly synchronize data between the
buffer and the CPU cache at the correct time, precisely the rule PHASE's
`sync` requirement encodes at the type level instead of leaving it to
driver-author discipline. The Linux kernel's own `dma-buf` subsystem
documentation independently confirms the same requirement exists at the
API level: CPU access must be bracketed by explicit synchronization calls
(the `DMA_BUF_IOCTL_SYNC` ioctl), and a 2025 kernel fix (CVE-2025-38095)
was needed specifically because that synchronization could race across
CPUs without an explicit memory barrier.

**What PHASE does differently:** the requirement to synchronize before
touching a DMA-owned buffer is checked at compile time by the domain
analyzer (M2), not left to the driver author to remember or to a static
analyzer to find after the fact.

---

## #2 — `move`/`destroy` while a borrow is live, and #4 — use after destroy

**`examples/bug_gallery/move_while_borrowed.phase`**,
**`examples/bug_gallery/use_after_destroy.phase`**

**Category:** [CWE-416](https://cwe.mitre.org/data/definitions/416.html) —
Use After Free.

**Real-world precedent:** CVE-2024-43856 is a 2024 Linux kernel race
condition in `dmam_free_coherent()`, the function that releases
DMA-allocated memory. It was caused by improper operation ordering when
freeing DMA allocations and managing their associated resources, which
allowed a device-managed resource entry to be freed prematurely while
still in use elsewhere — exactly the "the underlying memory was released
while something still held a reference to it" pattern bug #4 models
directly (a buffer destroyed while a caller still expects to use it), and
bug #2 prevents pre-emptively (disallowing the destroy/move in the first
place while any reference — a borrow — is still outstanding).

**What PHASE does differently:** ownership and borrow state are tracked
per-entity through the whole function body (M2); `move`, `sync`, and
`destroy` are all rejected outright while any borrow is outstanding, so
the "freed while referenced" window this CVE class depends on doesn't
exist in a program that passed analysis.

---

## #3 — Conflicting write borrows

**`examples/bug_gallery/write_borrow_conflict.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html) —
Race Condition, and [CWE-667](https://cwe.mitre.org/data/definitions/667.html)
— Improper Locking (MITRE's own description: the product does not
properly acquire or release a lock on a resource, leading to unexpected
resource state changes and behaviors).

**Real-world precedent:** this is the general shape behind a large class
of shared-buffer data races in concurrent and driver code — two writers
(or a writer and readers) with no exclusivity discipline enforced between
them. PHASE's borrow rule (many concurrent readers, one exclusive writer,
never both) is the same discipline Rust's own borrow checker enforces for
ordinary memory, applied here to buffers that can additionally live in a
hardware-owned domain.

**What PHASE does differently:** borrow conflicts are a compile-time
error (M2), not a runtime race that only manifests under specific timing.

---

## #5 — Skipped typestate transition

**`examples/bug_gallery/skipped_state_transition.phase`**

**Category:** [CWE-696](https://cwe.mitre.org/data/definitions/696.html) —
Incorrect Behavior Order.

**Real-world precedent:** the canonical real-world instance of this exact
bug shape is Apple's 2014 "goto fail" vulnerability, CVE-2014-1266. A
duplicated `goto fail;` statement caused a required signature-verification
step to be unconditionally skipped in the TLS handshake, so that vital
signature-checking code never ran and both valid and forged signatures
were accepted — completely breaking SSL/TLS certificate validation for
affected versions of iOS and OS X. In PHASE's terms: code was allowed to
proceed to the "verified" stage of a protocol object without actually
having passed through the "verify" step. PHASE's typestate system makes
the equivalent mistake — calling `validate()` on a packet that never went
through `decode()` — a compile error instead of a runtime security
failure, because a value's typestate is tracked and checked at every call
site (M3).

---

## #6 — Branch merge disagreement ("ENTITY_PHI")

**`examples/bug_gallery/branch_domain_disagreement.phase`**

**Category:** [CWE-696](https://cwe.mitre.org/data/definitions/696.html) —
Incorrect Behavior Order (the general "related behaviors performed in an
inconsistent order across different paths" class), and adjacent to
[CWE-691](https://cwe.mitre.org/data/definitions/691.html) — Insufficient
Control Flow Management.

**Real-world precedent:** this is the general shape behind a well-known
family of bugs where a resource, credential check, or piece of state is
correctly handled on one code path (say, the happy path) but a different,
less-tested path — an error branch, an early return, an alternate
condition — leaves it in a different, unaccounted-for state. This is
exactly the class of bug that's hard to catch in review, because each
branch looks correct read in isolation; the bug only exists in the
*disagreement* between branches.

**What PHASE does differently:** the M4 analyzer checks each branch
starting from the same pre-branch state and requires every incoming path
to agree on an entity's domain, typestate, and destroyed-ness before
letting the branches rejoin — turning "the paths quietly disagree" from a
runtime discovery into a compile error.

---

## #7 — Volatile MMIO reordering/elimination

**`examples/mmio_registers.phase`** (positive control —
see [`demos/m6_volatile_proof/`](../demos/m6_volatile_proof/) for the
adversarial before/after demo)

**Category:** [CWE-758](https://cwe.mitre.org/data/definitions/758.html) —
Reliance on Undefined, Unspecified, or Implementation-Defined Behavior.

**Real-world precedent:** ARM's own compiler documentation describes this
exact failure mode as a known, recurring class of bug: higher
optimization levels can reveal problems that aren't apparent at lower
optimization levels, including missing `volatile` qualifiers, manifesting
as code stuck polling hardware forever or timing-delay loops being
deleted outright. This is well-documented, recurring embedded-systems
folklore precisely because compilers are, by the C standard, *entitled*
to eliminate or reorder any non-`volatile` memory access they can prove
has no externally-visible effect — which is exactly wrong for a hardware
register, and exactly why the demo in `demos/m6_volatile_proof/`
reproduces a real instance of it happening.

**What PHASE does differently:** every `@MMIO`-tagged register is
unconditionally lowered to `volatile` storage by the code generator (M6)
— not something the programmer has to remember to write, or a reviewer
has to remember to check for.

---

## v1.0 additions

The entries below were added after an audit of the v0.1 analyzer found
programs it accepted that each of the rules above was supposed to reject.
Each is the same bug class as an earlier entry, reached by a route the
v0.1 checker did not close.

## #8 — Forging a typestate with an annotation

**`examples/bug_gallery/forged_typestate.phase`**

**Category:** [CWE-696](https://cwe.mitre.org/data/definitions/696.html) —
Incorrect Behavior Order.

**Real-world precedent:** the same "goto fail" shape as #5
(CVE-2014-1266), and arguably closer to it: the code *asserts* that
verification happened rather than calling the verifier out of order.
In v0.1, `let p: Packet<Validated> = receive_packet();` was accepted —
the annotation overrode the call's declared return state.

**What PHASE does now:** an annotation is a claim checked against
evidence, never a source of it. A state can only come from a function's
declared return state, or — for a freshly constructed value — the state
machine's initial state.

---

## #9 — Reusing a stale typestate value

**`examples/bug_gallery/stale_typestate_reuse.phase`**

**Category:** [CWE-672](https://cwe.mitre.org/data/definitions/672.html) —
Operation on a Resource after Expiration or Release.

**Real-world precedent:** the general replay/double-processing pattern —
an object that has already advanced through a protocol is processed again
through a second path from its old state (e.g. a packet decoded twice, a
one-time token accepted twice).

**What PHASE does now:** typestate values are linear. Passing one to a
parameter that requires a state, or returning it, consumes it; the old
name is dead. A value consumed on only one branch of an `if` cannot be
used after the join, and consuming a value created outside a `while` loop
inside its body is rejected (the next iteration would reuse it).

---

## #10 — Destroying a buffer the DMA engine still owns

**`examples/bug_gallery/destroy_while_dma.phase`**

**Category:** [CWE-416](https://cwe.mitre.org/data/definitions/416.html) —
Use After Free (the device is the user).

**Real-world precedent:** this is the most direct match for
CVE-2024-43856 (#2/#4 above): DMA memory released while the device side
still holds it. v0.1 only blocked `destroy` while a *CPU* borrow was
live; a buffer owned by `@DMA`/`@DEVICE` could be destroyed freely.

**What PHASE does now:** `destroy` requires the entity to be CPU-owned
(`sync` first).

---

## #11 — A hardware-owned buffer escaping its function

**`examples/bug_gallery/dma_escapes_function.phase`**

**Category:** [CWE-416](https://cwe.mitre.org/data/definitions/416.html)
and [CWE-825](https://cwe.mitre.org/data/definitions/825.html) — Expired
Pointer Dereference.

**Real-world precedent:** a transfer started on function-local storage
that outlives the function — the classic embedded bug of pointing a DMA
channel at a stack buffer and returning. Same "freed while the device
still uses it" class as #10, reached by scope exit instead of `destroy`.

**What PHASE does now:** every `return` (including the implicit one at the
end of a body) checks exit obligations (spec §4.2, lifetime checker): no
local may still be `@DMA`/`@DEVICE`-owned, no borrow may be live, and
parameters must be back in the domain the caller handed them over in.

---

## #12 — Using the owner while it is borrowed, #13 — use after release

**`examples/bug_gallery/use_owner_while_borrowed.phase`**,
**`examples/bug_gallery/use_after_release.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html)
(#12) and [CWE-672](https://cwe.mitre.org/data/definitions/672.html) (#13).

**Real-world precedent:** the same exclusivity discipline as #3. v0.1
checked borrow-vs-borrow conflicts but let the owner be passed to a call
while a write borrow was live, and let a released handle keep being used
— so the "one writer" guarantee could be bypassed without ever taking a
second borrow.

**What PHASE does now:** while any borrow of an entity is live, the entity
is only accessible through its borrows; a released handle cannot be used,
moved, synced, destroyed, or re-borrowed.

---

## #14 — Aliased arguments

**`examples/bug_gallery/aliased_arguments.phase`**

**Category:** [CWE-758](https://cwe.mitre.org/data/definitions/758.html) —
Reliance on Undefined, Unspecified, or Implementation-Defined Behavior.

**Real-world precedent:** passing one buffer as both input and output of
a DSP kernel. A kernel written for distinct buffers (and especially one
whose C prototype uses `restrict`, where aliasing is undefined behavior)
silently produces wrong output in place.

**What PHASE does now:** the same storage may appear more than once in a
single call only when every occurrence is a read borrow.

---

## #15 — Borrowing a DMA-owned buffer

**`examples/bug_gallery/borrow_unsynced_dma.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html),
[CWE-367](https://cwe.mitre.org/data/definitions/367.html).

**Real-world precedent:** identical to #1 — CPU access before the
transfer is synchronized — but reached through `borrow` instead of a
direct call, which v0.1 did not check.

**What PHASE does now:** a `@DMA`/`@DEVICE`-owned entity cannot be
borrowed.

---

## #16 — Shadowing an in-flight buffer

**`examples/bug_gallery/shadowed_dma_buffer.phase`**

**Category:** [CWE-416](https://cwe.mitre.org/data/definitions/416.html)
via lost tracking (the hazard of #11).

**Real-world precedent:** redeclaring a name hides the original entity
from the checker. In v0.1 a second `buffer<..> buf` silently replaced a
`buf` still owned by `@DMA`, after which nothing could ever sync it.
`let y = x;` had the same effect for any entity.

**What PHASE does now:** names cannot be redeclared or shadowed within a
function, and entities cannot be copied with `let`.

---

## v1.1 additions

v1.1 lets a function signature declare a domain change on a parameter
(`@RAM -> @DMA`, `@DMA -> @RAM`; spec §3.11), so a transfer can be started
by one call and finished by another. That opens the same bug classes as
before across function boundaries; each entry below is how PHASE closes
them.

## #17 — A "wait" helper that doesn't wait

**`examples/bug_gallery/broken_sync_promise.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html),
[CWE-367](https://cwe.mitre.org/data/definitions/367.html).

**Real-world precedent:** the cross-function form of #1. Driver code
commonly wraps DMA completion in a helper; if the helper returns before
the transfer is synchronized, every caller that trusts it reads memory
the engine still owns — and none of them looks wrong in review.

**What PHASE does:** a PHASE function declared `@DMA -> @RAM` is checked to
leave the parameter in `@RAM` on every path, explicit `return`s included.

---

## #18 — A signature promising an impossible hand-off

**`examples/bug_gallery/illegal_signature_transition.phase`**

**Category:** [CWE-696](https://cwe.mitre.org/data/definitions/696.html) —
Incorrect Behavior Order.

**Real-world precedent:** skipping a required step in a hardware
ownership protocol — here, handing a DMA-owned buffer straight to another
device without the CPU-side synchronization in between.

**What PHASE does:** a declared change must be a legal transition in the
domain table (spec §2.4), the same rule `move` and `sync` obey, so even a
trusted `extern fn` can't claim one the model forbids.

---

## #19 — Reading between "start transfer" and "wait"

**`examples/bug_gallery/read_while_capture_in_flight.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html),
[CWE-367](https://cwe.mitre.org/data/definitions/367.html).

**Real-world precedent:** exactly #1 (Linux DMA-buffer sync discipline,
CVE-2025-38095), in the split-call form real drivers use: one function
programs the engine and returns, another waits for completion.

**What PHASE does:** after a call to a parameter declared `@RAM -> @DMA`,
the argument is `@DMA` in the caller, so any CPU use before the matching
wait is rejected — and the diagnostic points at the call that handed it
over.

---

## v1.2 additions

v1.2 adds `read`/`write` access modes on parameters and `swap` for
ping-pong double buffering (spec §3.12, §3.13).

## #20 — Writing through a read borrow, #21 — a `read` parameter handed to a writer

**`examples/bug_gallery/write_through_read_borrow.phase`**,
**`examples/bug_gallery/read_param_handed_to_writer.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html) —
the exclusivity discipline of #3: the harm is memory changing under a
reader that believes it stable.

**Real-world precedent:** the same shared-buffer race pattern as #3: a
reader holds a view it believes stable while something else writes the
memory. Up to 1.1, PHASE's borrow rule had this exact hole: a read borrow
could be passed to any parameter, including one that writes.

**What PHASE does now:** read-only data may only reach `read` parameters
(a parameter with no mode gets a warning, so existing code keeps
compiling), and a `read` buffer is a `const` pointer in the generated C,
so the C compiler also rejects an extern body that writes through it.

---

## #22 — Refilling the buffer being processed

**`examples/bug_gallery/refill_while_processing.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html),
[CWE-367](https://cwe.mitre.org/data/definitions/367.html).

**Real-world precedent:** the classic double-buffering mistake in
streaming drivers: restarting the DMA transfer into the buffer the CPU is
about to read, so the engine overwrites the frame mid-processing. The
result is a torn frame — silently corrupted data, not a crash.

**What PHASE does:** the restart moves the buffer to `@DMA`, so the
processing call that follows is the use-before-sync error (#1), pointing
at the call that handed it back to the hardware.

---

## #23 — Double buffering without trading places, #24 — swapping a borrowed buffer

**`examples/bug_gallery/ping_pong_without_swap.phase`**,
**`examples/bug_gallery/swap_while_borrowed.phase`**

**Category:** [CWE-696](https://cwe.mitre.org/data/definitions/696.html)
(#23), [CWE-362](https://cwe.mitre.org/data/definitions/362.html) (#24).

**Real-world precedent:** a ping-pong loop whose roles drift out of sync
with the code waits on the idle buffer and reprograms a transfer that is
already running; a view kept across the swap ends up pointing at the
other buffer.

**What PHASE does:** the loop invariant (spec §4.3) fails unless the body
leaves both buffers as it found them — `swap` is how a correct loop
restores it — and `swap` is rejected while either buffer is borrowed.

---

## v1.3 additions

v1.3 added a runtime-checked build mode and a differential fuzzer (spec
§6.7). #25 and #26 are holes the fuzzer found in the analyzer; #27–#29
come from checking buffer types at call sites, which PHASE had not done.

## #25 — A buffer abandoned to DMA at the end of a block

**`examples/bug_gallery/dma_into_dead_block.phase`**

**Category:** [CWE-416](https://cwe.mitre.org/data/definitions/416.html),
[CWE-825](https://cwe.mitre.org/data/definitions/825.html).

**Real-world precedent:** the block-scoped form of #11: a transfer started
into storage that goes out of scope, so the engine writes into stack
memory the program is already reusing.

**What PHASE does now:** entities declared in an `if`/`while` block must
be CPU-owned when the block falls through to its end, exactly as at a
function return. Up to 1.2 only function exits were checked.

---

## #26 — Swapping with a buffer from an inner block

**`examples/bug_gallery/swap_across_blocks.phase`**

**Category:** [CWE-825](https://cwe.mitre.org/data/definitions/825.html) —
Expired Pointer Dereference.

**Real-world precedent:** a long-lived handle redirected to short-lived
storage — the classic dangling pointer. `swap` exchanges storage, so the
outer name ended up pointing into a block that had ended.

**What PHASE does now:** `swap` requires both buffers to be declared in
the same block.

---

## #27 — A shorter buffer where a longer one is required, #28 — the wrong element type, #29 — a run-time length where a fixed one is assumed

**`examples/bug_gallery/buffer_length_mismatch.phase`**,
**`examples/bug_gallery/buffer_element_mismatch.phase`**,
**`examples/bug_gallery/runtime_length_to_fixed.phase`**

**Category:** [CWE-787](https://cwe.mitre.org/data/definitions/787.html) —
Out-of-bounds Write (#27, #29), and
[CWE-843](https://cwe.mitre.org/data/definitions/843.html) — Access of
Resource Using Incompatible Type ("Type Confusion") (#28).

**Real-world precedent:** the most common memory-safety bug class there
is: a buffer handed to code that assumes it is bigger, or holds a
different type, than it does. In PHASE's generated C a callee passes the
length *it declared* on to kernels, so before 1.3 a 64-element buffer
passed where 256 were declared became a 256-element write into 64 slots.

**What PHASE does now:** every buffer argument must match its parameter's
element type and length. A parameter can accept any length with
`buffer<T, _>`, in which case the real length is what travels with it at
run time; such a buffer can only be passed on to another `_` parameter.

---

## v1.4 additions

v1.4 added `rotate` for rings of buffers and `split`/`join` for buffers a
DMA engine fills in parts (spec §3.14). These four are the ways circular
and triple-buffered streaming goes wrong.

## #30 — Processing the half the engine is still filling

**`examples/bug_gallery/process_half_in_flight.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html),
[CWE-367](https://cwe.mitre.org/data/definitions/367.html).

**Real-world precedent:** circular DMA with half-transfer interrupts is
how most microcontroller ADC and audio drivers stream (on STM32, the HAL's
`HAL_ADC_ConvHalfCpltCallback` / `HAL_ADC_ConvCpltCallback` pair). The
callback for one half must process *that* half; processing the other one
reads a block the engine is in the middle of writing — a torn block, the
circular form of #22.

**What PHASE does:** after `split ring into lo, hi;` each half is tracked
on its own, so the half the engine owns is `@DMA` and can't be passed to
a kernel that reads RAM.

---

## #31 — Using the whole buffer while it is split, #32 — joining it with a half in flight

**`examples/bug_gallery/use_ring_while_split.phase`**,
**`examples/bug_gallery/join_with_half_in_flight.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html),
[CWE-367](https://cwe.mitre.org/data/definitions/367.html).

**Real-world precedent:** a whole-buffer view (a summary, a log dump, a
copy-out) of a circular buffer while the engine still owns part of it;
or stopping the stream for one half only and then treating the buffer as
whole. Either way the CPU reads memory that is still being written.

**What PHASE does:** a split buffer can't be used at all until its parts
are joined back, and `join` requires every part — in its original place —
to be in the same domain, which the buffer then has.

---

## #33 — Rotating a triple-buffer ring the wrong way

**`examples/bug_gallery/rotate_wrong_direction.phase`**

**Category:** [CWE-696](https://cwe.mitre.org/data/definitions/696.html) —
Incorrect Behavior Order.

**Real-world precedent:** the three-buffer form of #23: the code's idea of
which buffer is free, which is filling and which is next drifts from the
hardware's, so the loop waits on a buffer nothing is filling and
restarts one already in flight.

**What PHASE does:** the loop invariant — a `while` body must leave every
buffer as it found it — rejects a rotation that leaves the roles shifted.

---

## v2.0 additions

v2.0 added `device` register maps (spec §3.7): a peripheral's registers,
their access modes, widths and bit fields, declared once and checked at
every access. #34–#38 are the register mistakes that checking catches;
#39 is the 1.2 warning that became an error.

## #34 — Clearing a flag through a read-only register

**`examples/bug_gallery/write_read_only_register.phase`**

**Category:** [CWE-684](https://cwe.mitre.org/data/definitions/684.html) —
Incorrect Provision of Specified Functionality.

**Real-world precedent:** peripherals differ in how a status flag is
cleared — by writing 1 to it, by writing a separate clear register, by
reading a data register. Code written for one convention and run on
another writes a register the hardware ignores, and the flag it believes
it cleared is still set on the next poll.

**What PHASE does:** a register declared `read` cannot be written.

---

## #35 — Reading a write-only register, #36 — setting one field of it

**`examples/bug_gallery/read_write_only_register.phase`**,
**`examples/bug_gallery/field_write_on_write_only_register.phase`**

**Category:** [CWE-908](https://cwe.mitre.org/data/definitions/908.html) —
Use of Uninitialized Resource.

**Real-world precedent:** a write-only command or clear register reads
back as zero, as the last value on the bus, or as something unrelated.
Code that reads it gets a meaningless value; the C idiom `CMD |= START;`
reads it too, and writes that value back with one bit changed —
clobbering every other field.

**What PHASE does:** a register declared `write` cannot be read, and a
single field of it cannot be written (a field write is read-modify-write);
the whole register must be written instead.

---

## #37 — A value too wide for its field

**`examples/bug_gallery/field_value_too_wide.phase`**

**Category:** [CWE-197](https://cwe.mitre.org/data/definitions/197.html) —
Numeric Truncation Error.

**Real-world precedent:** a 4-bit prescaler written with 16. In C,
`CTRL |= 16 << PRESCALE_POS;` sets the bit above the field — here the
timer's enable bit; with a mask, it silently writes 0.

**What PHASE does:** a literal written to a register or field must fit
its width. (A value computed at run time is masked to the field.)

---

## #38 — Two registers at one address

**`examples/bug_gallery/overlapping_registers.phase`**

**Category:** [CWE-684](https://cwe.mitre.org/data/definitions/684.html).

**Real-world precedent:** a copy-pasted line in a hand-written register
header, so two names refer to one address and every write to one changes
the other.

**What PHASE does:** registers in a device may not overlap, must be
aligned to their size, and fields may not overlap; the generated C checks
every offset with `_Static_assert`.

---

## #39 — Read-only data to a parameter that never said it only reads

**`examples/bug_gallery/read_only_to_unannotated_param.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html).

**Real-world precedent:** as #20 and #21 — a writer reached through a
read-only view — but through a function declared before access modes
existed, which for all the checker knows writes what it is given.

**What PHASE does:** since 2.0, an error (a warning in 1.2–1.4). One word
in the signature, `read`, fixes it when the function really only reads.

---

## v2.1 additions

v2.1 added `device type`s (spec §3.7) and `import` (§3.15), so a board's
devices are declared once, in a file programs share — and checked
against each other.

## #40 — Two devices at one address

**`examples/bug_gallery/overlapping_devices.phase`**

**Category:** [CWE-684](https://cwe.mitre.org/data/definitions/684.html).

**Real-world precedent:** #38 at the scale of a whole peripheral: a
board-support header with a copy-pasted base address, so two driver
instances program the same UART — configuring one silently reconfigures
the other.

**What PHASE does:** every device spans from its base address to the end
of its last register, and no two devices may overlap; a device's base
must also keep its registers aligned.

---

## v2.2 additions

v2.2 added interrupt handlers, events and critical sections (spec §3.16).
These two are the classic ways interrupt code goes wrong.

## #41 — A read-modify-write an interrupt can split

**`examples/bug_gallery/interrupted_field_write.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html) —
Concurrent Execution using Shared Resource with Improper Synchronization.

**Real-world precedent:** `REG |= BIT;` in thread code on a register an
interrupt handler also changes — one of the most common embedded race
conditions, and the reason CMSIS and every RTOS provide critical-section
macros. The handler runs between the load and the store, and the store
writes its change away.

**What PHASE does:** follows the call graph from `main` and from every
handler; a field write in thread code to a register handler code writes
must be inside `critical { }`.

---

## #42 — Waiting inside an interrupt handler

**`examples/bug_gallery/wait_in_handler.phase`**

**Category:** [CWE-833](https://cwe.mitre.org/data/definitions/833.html) —
Deadlock.

**Real-world precedent:** a handler (or a helper it calls) that blocks on
something only another interrupt — or the code it interrupted — can
deliver: the system hangs with that priority level masked.

**What PHASE does:** a `wait` reachable from any handler, or inside
`critical`, is an error, with the call chain from the handler.

---

## #43 — An event handing back the wrong half

**`examples/bug_gallery/wrong_half_event.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html) —
Concurrent execution using a shared resource with improper
synchronization; [CWE-367](https://cwe.mitre.org/data/definitions/367.html)
— time-of-check to time-of-use.

**Real-world precedent:** the half-transfer / transfer-complete pair of a
circular DMA stream (STM32 HAL's `HalfCpltCallback` and `CpltCallback`)
handled the wrong way round: on "first half done", the code reads the
second half — which the engine is writing at that moment. The data looks
plausible and is torn, and nothing crashes.

**What PHASE does:** `wait E for b` hands `b` back when the interrupt
signals `E`. Within a function an event hands back one buffer: the first
`wait half_done for lo` fixes it, and `wait half_done for hi` later is an
error. (Both halves are with the engine here, so ownership alone can't
tell them apart.)

---

## #44 — Waiting for an event nothing signals

**`examples/bug_gallery/wait_never_signalled.phase`**

**Category:** [CWE-835](https://cwe.mitre.org/data/definitions/835.html) —
Loop with unreachable exit condition.

**Real-world precedent:** a flag-based handshake whose setter was removed
or never written — the receive handler still acknowledges the interrupt,
but no longer sets the "data ready" flag — so the main loop sleeps
forever; on the bench the device just stops responding.

**What PHASE does:** a `wait E` is an error when no code reachable from
`main` or a handler signals `E`. (A host build also stops a `wait` that
1,000 rounds of handlers don't satisfy, for signals that exist but never
run.)

---

## #45 — A named value too wide for its field

**`examples/bug_gallery/constant_too_wide.phase`**

**Category:** [CWE-197](https://cwe.mitre.org/data/definitions/197.html) —
Numeric truncation error.

**Real-world precedent:** a prescaler or divider constant that holds the
divider where the field wants the divider minus one, or a mask computed
one bit too wide. `CTRL |= DIV << 4` then sets a bit of the next field
(here, the timer's interrupt enable), and nothing complains.

**What PHASE does:** computes every constant expression when it compiles
the program and checks the value against the register or field it is
written to, as it does a literal.

---

## #46 — A field write that clears another pending flag

**`examples/bug_gallery/field_write_clears_flag.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html) —
Concurrent execution using a shared resource with improper
synchronization.

**Real-world precedent:** `UART->INTSTATUS |= TX` on a write-1-to-clear
status register. The read returns RX set as well; writing it back clears
RX, and the receive interrupt is gone before anyone handled it. The same
bug with write-0-to-clear flags (`TIMx->SR &= ~UIF` on an STM32) clears
a flag set between the read and the write, which is why ST's reference
manuals say never to read-modify-write those registers.

**What PHASE does:** fields can be declared `w1c`, `rc` or `rc_w0`; a
field write that would clear another flag, race one, or read a
read-clear register is an error, and the fix — a whole-register write —
is what the message asks for.

---

## #47 — One past the last element of a register array

**`examples/bug_gallery/register_index_out_of_range.phase`**

**Category:** [CWE-787](https://cwe.mitre.org/data/definitions/787.html) —
Out-of-bounds write.

**Real-world precedent:** `DMA2->S[8].CR` on a controller with streams 0
to 7 — an off-by-one in a stream or channel number, from a table or a
define. In C it compiles and writes past the controller's registers.

**What PHASE does:** register arrays are declared with their count, and
an index — which must be a constant — is checked against it.

---

## #48 — Refilling a DMA source while it is read

**`examples/bug_gallery/dma_source_rewritten.phase`**

**Category:** [CWE-362](https://cwe.mitre.org/data/definitions/362.html),
[CWE-367](https://cwe.mitre.org/data/definitions/367.html).

**Real-world precedent:** a memory-to-peripheral or memory-to-memory DMA
started on a buffer the code then refills for the next transfer before
the current one completes; the device receives a mix of two blocks.

**What PHASE does:** writing a buffer's address into an `address`
register hands the buffer to the hardware, so writing it afterwards is
the same error as any other use of a buffer the DMA engine owns, with a
note at the register write.

---

## Summary table

| # | Bug-gallery file | CWE | Named real-world precedent |
|---|---|---|---|
| 1 | `use_before_sync.phase` | CWE-362, CWE-367 | Linux DMA-buffer sync discipline (USENIX Sec '21; CVE-2025-38095) |
| 2 | `move_while_borrowed.phase` | CWE-416 | CVE-2024-43856 (Linux `dmam_free_coherent` race) |
| 3 | `write_borrow_conflict.phase` | CWE-362, CWE-667 | general shared-buffer race pattern |
| 4 | `use_after_destroy.phase` | CWE-416 | CVE-2024-43856 |
| 5 | `skipped_state_transition.phase` | CWE-696 | CVE-2014-1266 (Apple "goto fail") |
| 6 | `branch_domain_disagreement.phase` | CWE-696, CWE-691 | general branch-inconsistent-state pattern |
| 7 | volatile MMIO (`mmio_registers.phase`) | CWE-758 | documented in ARM's own compiler docs |
| 8 | `forged_typestate.phase` | CWE-696 | CVE-2014-1266 (Apple "goto fail") |
| 9 | `stale_typestate_reuse.phase` | CWE-672 | general replay/double-processing pattern |
| 10 | `destroy_while_dma.phase` | CWE-416 | CVE-2024-43856 |
| 11 | `dma_escapes_function.phase` | CWE-416, CWE-825 | DMA into an out-of-scope stack buffer |
| 12 | `use_owner_while_borrowed.phase` | CWE-362 | general shared-buffer race pattern |
| 13 | `use_after_release.phase` | CWE-672 | general use-after-release pattern |
| 14 | `aliased_arguments.phase` | CWE-758 | in-place call of a non-aliasing (`restrict`) kernel |
| 15 | `borrow_unsynced_dma.phase` | CWE-362, CWE-367 | same as #1 |
| 16 | `shadowed_dma_buffer.phase` | CWE-416 | lost tracking of an in-flight buffer |
| 17 | `broken_sync_promise.phase` | CWE-362, CWE-367 | cross-function form of #1 |
| 18 | `illegal_signature_transition.phase` | CWE-696 | skipped step in a hardware hand-off |
| 19 | `read_while_capture_in_flight.phase` | CWE-362, CWE-367 | same as #1, split across calls |
| 20 | `write_through_read_borrow.phase` | CWE-362 | general shared-buffer race pattern (as #3) |
| 21 | `read_param_handed_to_writer.phase` | CWE-362 | general shared-buffer race pattern (as #3) |
| 22 | `refill_while_processing.phase` | CWE-362, CWE-367 | torn frame in a double-buffered stream |
| 23 | `ping_pong_without_swap.phase` | CWE-696 | double-buffer roles out of sync with the code |
| 24 | `swap_while_borrowed.phase` | CWE-362 | a view left pointing at the wrong buffer |
| 25 | `dma_into_dead_block.phase` | CWE-416, CWE-825 | DMA into out-of-scope storage (as #11), found by fuzzing |
| 26 | `swap_across_blocks.phase` | CWE-825 | dangling name after a swap, found by fuzzing |
| 27 | `buffer_length_mismatch.phase` | CWE-787 | buffer overflow from a size mismatch |
| 28 | `buffer_element_mismatch.phase` | CWE-843 | type confusion between byte and sample buffers |
| 29 | `runtime_length_to_fixed.phase` | CWE-787 | unchecked length assumption |
| 30 | `process_half_in_flight.phase` | CWE-362, CWE-367 | torn block in a circular (half-transfer) DMA stream |
| 31 | `use_ring_while_split.phase` | CWE-362, CWE-367 | whole-buffer view over a half still being written |
| 32 | `join_with_half_in_flight.phase` | CWE-362, CWE-367 | stream stopped for one half only |
| 33 | `rotate_wrong_direction.phase` | CWE-696 | triple-buffer roles out of sync with the code |
| 34 | `write_read_only_register.phase` | CWE-684 | a write the hardware ignores; a flag never cleared |
| 35 | `read_write_only_register.phase` | CWE-908 | trusting what a write-only register reads back as |
| 36 | `field_write_on_write_only_register.phase` | CWE-908 | `CMD \|= START` on a write-only register |
| 37 | `field_value_too_wide.phase` | CWE-197 | a field value spilling into (or masked out of) its register |
| 38 | `overlapping_registers.phase` | CWE-684 | a copy-pasted register map |
| 39 | `read_only_to_unannotated_param.phase` | CWE-362 | a legacy API that may write what it is given |
| 40 | `overlapping_devices.phase` | CWE-684 | a copy-pasted base address in a board file |
| 41 | `interrupted_field_write.phase` | CWE-362 | `REG \|= BIT` racing an interrupt handler |
| 42 | `wait_in_handler.phase` | CWE-833 | a handler blocking on an interrupt that can't run |
| 43 | `wrong_half_event.phase` | CWE-362, CWE-367 | half-transfer and transfer-complete callbacks swapped |
| 44 | `wait_never_signalled.phase` | CWE-835 | a "data ready" flag no handler sets any more |
| 45 | `constant_too_wide.phase` | CWE-197 | a divider constant that spills into the next field |
| 46 | `field_write_clears_flag.phase` | CWE-362 | `INTSTATUS \|= TX` on write-1-to-clear flags |
| 47 | `register_index_out_of_range.phase` | CWE-787 | `DMA2->S[8]` on an 8-stream controller |
| 48 | `dma_source_rewritten.phase` | CWE-362, CWE-367 | a DMA source refilled mid-transfer |
