# Changelog

All notable changes to PHASE are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
uses [Semantic Versioning](https://semver.org/).

## [1.0.0] — 2026-10-12

The first stable release. An audit of the v0.1 analyzer found programs
it accepted that broke the very rules the bug gallery documents; 1.0
closes all of them, states the resulting guarantees precisely
(`phase_specification.md` §4.4), and pins them with tests.

Because the checker is stricter, **some programs v0.1 accepted are now
rejected**, each one for a real bug. The fix for each is listed below.

### Fixed — soundness

- **Typestate could be forged with an annotation.**
  `let p: Packet<Validated> = receive_packet();` was accepted; the
  annotation overrode the call's declared return state. An annotation is
  now checked against its initializer. A new value may only be created
  directly (struct literal or no initializer) in the machine's *initial*
  state. *Fix:* call the transition functions.
- **Typestate values could be reused.** After `decode(p1)`, `p1` was
  still usable. Typestate values are now linear: passing one to a
  state-requiring parameter, or returning it, consumes it. Consumption on
  one branch of an `if` makes the value unusable after the join, and
  consuming an outer value inside a `while` body is an error.
- **A buffer could be destroyed while `@DMA`/`@DEVICE` owned it.**
  *Fix:* `sync` before `destroy`.
- **Functions could return while hardware owned a local buffer**, with
  borrows still live, or with a parameter left in a different domain
  than it arrived in. These are now checked at every `return`, including
  the implicit one at the end of a body. Parameters cannot be destroyed.
- **The owner of a borrowed buffer could still be used directly**, so a
  write borrow wasn't exclusive. While any borrow is live, the owner is
  reachable only through its borrows.
- **Released borrow handles could still be used**, and handles could be
  moved, synced, destroyed, or re-borrowed. All rejected now.
- **A `@DMA`/`@DEVICE`-owned buffer could be borrowed**, bypassing `sync`.
- **The same buffer could be passed twice to one call** (e.g. as a
  kernel's input and output). Now allowed only when every occurrence is a
  read borrow.
- **Names could be redeclared**, hiding a live (even DMA-owned) entity,
  and `let y = x;` could copy an entity into an untracked alias. Both are
  rejected.
- **Unknown names in conditions passed `check`** and only failed later in
  `cc`. Identifiers in conditions and assignments must now exist and be
  scalars. Call arity and duplicate function names are also checked.
- A branch ending in `return` no longer takes part in the join after an
  `if` (previously it could cause spurious disagreement errors or hide
  real ones); statements after `return` are reported as unreachable.

### Added

- Bug gallery entries #8–#16: `forged_typestate`, `stale_typestate_reuse`,
  `destroy_while_dma`, `dma_escapes_function`, `use_owner_while_borrowed`,
  `use_after_release`, `aliased_arguments`, `borrow_unsynced_dma`,
  `shadowed_dma_buffer`, with CWE mappings in `docs/security_mapping.md`.
- The gallery test now asserts each file is rejected with the exact error
  in its `// Expected:` header.
- 46 analyzer regression tests, each rule paired with a program it must
  keep accepting.
- `phase --version` / `-V` and `phase --help` / `-h`.
- CLI integration tests that drive the real `phase` binary.
- GitHub Actions CI: `cargo fmt --check`, `clippy -D warnings`, the full
  test suite with the ARM cross-compiler and QEMU installed
  (`PHASE_REQUIRE_HW_TOOLS=1` turns the hardware test's skip into a
  failure), the `-O2` volatile proof demo, and a staleness check on the
  bug-gallery transcript.

### Changed

- `phase build` no longer has to run from the repository root. It finds
  the nearest `runtime/` at or above the source file (override:
  `PHASE_RUNTIME_DIR`) and writes to the `build/` beside it.
- Identical diagnostics are reported once, and a rejected `borrow` no
  longer produces a follow-on error at its `release`.
- Loop-related merge errors now explain that a `while` body must leave
  every entity as it found it.
- All crates are versioned together via `[workspace.package]`.
- The specification is updated to 1.0: §4.4 states the guarantees and the
  `extern fn` trust boundary, and stale v0.1 text (crate layout, CLI,
  runtime description, domain count, pipeline) now matches the code.
- The README no longer carries a hand-maintained test count.
- `demos/m6_volatile_proof/build/*.s` are no longer tracked (they are
  toolchain-dependent build output).

## [0.1.0]

The original milestone series, M1–M11: lexer, parser, domain/ownership/
borrow analysis, typestate, `if`/`while` merging, PIR and C codegen with a
simulated runtime, `volatile` MMIO with an `-O2` proof, real control-flow
codegen, scalar assignment, a real ARM Cortex-M3 runtime validated under
QEMU, and `volatile_read` in loop conditions.
