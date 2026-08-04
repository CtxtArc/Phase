# PHASE

A physical-domain safety language for signal/RF/embedded pipelines.
Full design rationale and roadmap: see `phase_specification.md`.

## Status: M4 — Control Flow + Branch Merging ("ENTITY_PHI") (done)

Builds on M3. Adds `if`/`else`/`while` to the grammar and the hardest piece
of the analyzer: checking that an entity's domain, typestate, and
destroyed-ness agree across every incoming path before the branches join.

- `if cond { .. } else { .. }`, `else if` chains, and `while cond { .. }`.
- A real parsing subtlety, handled the same way Rust handles it: `if flag
  { .. }` is ambiguous with the struct literal `flag { .. }`, so conditions
  are parsed with struct-literal parsing disabled (`Parser::restrict_struct_lit`).
- **The merge check (bug-gallery #6):** each branch is checked starting
  from the *same* pre-branch state; at the join point, every variable that
  existed before the branch must agree on domain, typestate, and
  destroyed-ness across all incoming paths, or it's rejected with e.g.
  `entity 'x' disagrees on domain across incoming branches (@DMA vs
  @DEVICE)`. An `if` with no `else` is checked against the implicit
  "unchanged" path, which is what makes `if flag { move x -> @DMA; }` (no
  matching path back to `@RAM`) a compile error even without an `else` at
  all.
- **Proper scoping falls out for free:** a variable declared inside a
  branch is simply not in the pre-branch state, so it's never carried into
  the merged result — using it after the branch is an "unknown entity"
  error, exactly as it should be.
- **Borrows inside a branch:** a borrow whose target wasn't already
  borrowed before the branch must be fully released before the branch
  ends (its handle variable is scoped to the branch and has no way to be
  released afterward otherwise). A borrow that existed before the branch
  must agree on its borrow state (released vs. still held) across all
  paths, same as any other property.
- **`while`, a documented scope cut:** a loop body is checked once, and
  its exit state is merged against the pre-loop state (as if the loop ran
  zero or one times). This is sound for the common case but does **not**
  compute a true fixed point across arbitrarily many iterations — e.g. a
  domain that oscillates with a period of two loop iterations wouldn't be
  caught. Precise fixed-point loop analysis is deferred as future work;
  this is called out explicitly rather than silently under-verified.

## Layout

```
crates/
  ast/       AST node definitions (no logic)
  lexer/     hand-written lexer, source -> Vec<Token>
  parser/    hand-written recursive-descent parser, tokens -> AST
  analysis/  M2 domain/ownership/borrow + M3 typestate + M4 branch-merge analyzer
  driver/    CLI binary ("phase")
examples/
  radio_pipeline.phase        the DMA/device pipeline example from the spec
  packet_pipeline.phase       the typestate example from the spec
  branching_pipeline.phase    branch-consistent domain handling (M4)
  bug_gallery/                one deliberately-broken file per bug class (#1-#6)
phase_specification.md        full language spec, roadmap, bug gallery
```

## Building & testing

```
cargo build --workspace
cargo test --workspace
```

Tests are plain `cargo test` — no custom test runner. 89 tests total across
the workspace. Each crate has unit tests in `src/lib.rs`
(`#[cfg(test)] mod tests`); the driver crate has integration tests in
`crates/driver/tests/parses_examples.rs` that run the real files under
`examples/` through the real parser and analyzer end to end, so
regressions anywhere in the pipeline are caught immediately.

## CLI

```
cargo run --bin phase -- check examples/radio_pipeline.phase
cargo run --bin phase -- check examples/branching_pipeline.phase
cargo run --bin phase -- check examples/bug_gallery/branch_domain_disagreement.phase
cargo run --bin phase -- --dump-ast examples/branching_pipeline.phase
```

## Next milestone (M5)

PIR generation + C codegen + the simulated `sim_dma`/`sim_adc`/`sim_dac`
runtime — the first milestone that produces an actual running binary
rather than only a static checker.



