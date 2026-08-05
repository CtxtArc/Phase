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
