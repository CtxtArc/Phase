# PHASE

A physical-domain safety language for signal/RF/embedded pipelines.
Full design rationale and roadmap: see `phase_specification.md`.

## Status: M2 — Domain/Ownership/Borrow Analyzer (done)

Builds on M1's lexer/parser/AST. Straight-line subset only (no `if`/`while`
yet — that's M4). The analyzer walks each `fn` body statement-by-statement
and enforces:

- every `move`/`sync` follows a legal domain transition (spec §2.4)
- an entity's domain must match what a call site requires before it's
  passed as an argument (this is how "reading a `@DMA` buffer without
  `sync`" gets caught — the call-site domain check *is* the enforcement)
- `move`/`sync`/`destroy` are rejected while a borrow is live
- borrows conflict correctly: many readers OK, a writer must be exclusive
- use of a destroyed entity is rejected

All four bug-gallery items from the spec (`examples/bug_gallery/*.phase`)
are caught with the exact error text documented in the spec, verified both
as analysis-crate unit tests and as end-to-end driver integration tests
that parse the real files on disk and run the real analyzer.

The analyzer intentionally degrades one bad statement at a time rather than
cascading: an invalid statement is treated as a no-op for state-tracking
purposes, so one real bug doesn't spam unrelated-looking follow-on errors.

## Layout

```
crates/
  ast/       AST node definitions (no logic)
  lexer/     hand-written lexer, source -> Vec<Token>
  parser/    hand-written recursive-descent parser, tokens -> AST
  analysis/  M2 domain/ownership/borrow analyzer
  driver/    CLI binary ("phase")
examples/
  radio_pipeline.phase        the running example from the spec
  bug_gallery/                one deliberately-broken file per bug class
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
cargo run --bin phase -- check examples/bug_gallery/use_before_sync.phase
cargo run --bin phase -- --dump-ast examples/radio_pipeline.phase
cargo run --bin phase -- --dump-tokens examples/radio_pipeline.phase
```

## Next milestone (M3)

State analyzer: implement typestate checking for `state` declarations
(`Packet<Received>` vs `Packet<Decoded>`), rejecting bug-gallery item #5
(calling a function that requires one state with a value proven to be in
another).

