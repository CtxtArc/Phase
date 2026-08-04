# PHASE

A physical-domain safety language for signal/RF/embedded pipelines.
Full design rationale and roadmap: see `phase_specification.md`.

## Status: M3 — Typestate Analyzer (done)

Builds on M2's domain/ownership/borrow analyzer. Adds typestate checking
for `state` declarations (spec §3.8):

```
state Packet {
    Received -> Decoded -> Validated
}
extern fn decode(p: Packet<Received>) -> Packet<Decoded>;
```

- `Name<State>` is now a real type (`TypeExpr::Stateful`), parsed anywhere
  a type can appear (fn params/returns, `let` annotations).
- Every function signature's stateful params/return are validated once
  against the declared `state` machines (unknown machine, unknown state,
  duplicate machine/state declarations all rejected).
- A local's typestate is tracked either from an explicit `Name<State>`
  annotation or inferred from a call's declared return typestate
  (`let p2 = decode(p1);` — `p2` is now known to be `Packet<Decoded>`).
- Passing a value in the wrong state to a function that requires a
  specific state is rejected with the spec's documented message:
  `expected state Decoded, found Received` — this is bug-gallery item #5.
- Function parameters are now registered as locals inside their own body
  (a gap M2 had — a body that used a parameter directly, not just ones
  re-bound through `let`, wasn't checked at all), so e.g. `return p;`
  inside a function claiming to return `Packet<Decoded>` is checked too.

## Layout

```
crates/
  ast/       AST node definitions (no logic)
  lexer/     hand-written lexer, source -> Vec<Token>
  parser/    hand-written recursive-descent parser, tokens -> AST
  analysis/  M2 domain/ownership/borrow analyzer + M3 typestate analyzer
  driver/    CLI binary ("phase")
examples/
  radio_pipeline.phase        the DMA/device pipeline example from the spec
  packet_pipeline.phase       the typestate example from the spec
  bug_gallery/                one deliberately-broken file per bug class (#1-#5)
phase_specification.md        full language spec, roadmap, bug gallery
```

## Building & testing

```
cargo build --workspace
cargo test --workspace
```

Tests are plain `cargo test` — no custom test runner. Each crate has unit
tests in `src/lib.rs` (`#[cfg(test)] mod tests`); the driver crate has
integration tests in `crates/driver/tests/parses_examples.rs` that run the
real files under `examples/` through the real parser and analyzer end to
end, so regressions anywhere in the pipeline are caught immediately.

## CLI

```
cargo run --bin phase -- check examples/radio_pipeline.phase
cargo run --bin phase -- check examples/packet_pipeline.phase
cargo run --bin phase -- check examples/bug_gallery/skipped_state_transition.phase
cargo run --bin phase -- --dump-ast examples/packet_pipeline.phase
```

## Next milestone (M4)

Control flow (`if`/`while`) + `ENTITY_PHI` merge checking — the hardest
milestone per the roadmap. An entity may have a different domain/state on
different incoming branches; the merge point must require all incoming
paths to agree, or reject with a clear "disagrees on domain/state across
incoming branches" error (bug-gallery item #6).


