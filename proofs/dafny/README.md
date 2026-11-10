# A mechanized soundness proof for PHASE's rules (in progress)

The fuzzer is evidence that the analyzer is sound; this directory is
working toward a proof of it. `PhaseCore.dfy` states PHASE's core rules
in [Dafny](https://dafny.org) and proves them sound against a run-time
semantics, so the guarantee holds for every program and every path, not
just the ones phase-fuzz happens to generate.

**Status: not yet machine-checked.** The file has not been run through
the verifier yet; until `dafny verify` passes, nothing here counts as a
proof.

## What it covers so far

- A core calculus: `Read`, `Write`, `Start` (`@RAM -> @DMA`), `Sync`,
  `Destroy`, sequencing, `if`/`else` and `while`.
- `Check`, a model of the analyzer's domain rules (rules 1 and 3) and its
  branch-merge and loop-invariant rules (rule 5).
- `Exec`, a big-step semantics that may take either branch of an `if` and
  run a loop any number of times, so a theorem about it covers every path.
- `Soundness`: a program `Check` accepts never touches, syncs or frees a
  buffer it does not own, on any path, and ends in exactly the domains
  the checker predicted.
- `FunctionExitSafety`: no function returns while the DMA engine still
  owns one of its buffers.
- Sanity lemmas: the bug-gallery shapes `use_before_sync`,
  `destroy_while_dma` and `branch_domain_disagreement` are rejected, and
  a correct capture loop is accepted.

## Running it

```
dafny verify proofs/dafny/PhaseCore.dfy
```

Dafny 4.x is needed (`dafny-bin` on the AUR, or
`dotnet tool install --global dafny`).

## Roadmap

1. **Memory and a hostile device.** Give the state real buffer contents
   and let the DMA engine overwrite any buffer it owns between any two CPU
   steps; prove noninterference — what an accepted program reads does not
   depend on what the device does. That is the formal meaning of "no DMA
   race".
2. **Borrows**: many readers or one writer; nothing moved or freed while
   borrowed.
3. **`swap`, `rotate`, `split`, `join`**: names that alias halves of one
   buffer. This is where phase-fuzz found real analyzer holes.
4. **Functions with domain-changing signatures**, checked per function
   and composed.
5. **Interrupts and `wait E for b`**, with the `critical { }` rule.
6. **Close the gap to the Rust analyzer**: compile `Check` with Dafny and
   differential-test it against `phase check` on phase-fuzz's programs.

The full list of rules being modeled is in
[`phase_specification.md`](../../phase_specification.md) §4.4.
