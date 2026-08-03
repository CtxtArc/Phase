# PHASE

A physical-domain safety language for signal/RF/embedded pipelines.
Full design rationale and roadmap: see `phase_specification.md`.

## Status: M1 — Lexer + Parser + AST (done)

Straight-line subset only (no `if`/`while` yet — that's M4). Covers:
entities, `state` (typestate) declarations, `fn` / `extern fn`, buffers,
`move` / `sync` / `borrow` / `release` / `destroy`, MMIO `volatile_read`/
`volatile_write` call syntax, expressions with normal arithmetic/comparison
precedence.

No semantic analysis yet — the parser accepts any *syntactically* valid
program, including ones that would violate physical rules (e.g. using a
`@DMA` buffer without `sync`). Rejecting those is the job of the M2+
analyzer.

## Layout

```
crates/
  ast/      AST node definitions (no logic)
  lexer/    hand-written lexer, source -> Vec<Token>
  parser/   hand-written recursive-descent parser, tokens -> AST
  driver/   CLI binary ("phase")
examples/
  radio_pipeline.phase   the running example from the spec
phase_specification.md   full language spec, roadmap, bug gallery
```

## Building & testing

```
cargo build --workspace
cargo test --workspace
```

Tests are plain `cargo test` — no custom test runner. Each crate has unit
tests in `src/lib.rs` (`#[cfg(test)] mod tests`); the driver crate has
integration tests in `crates/driver/tests/parses_examples.rs` that parse
`examples/radio_pipeline.phase` end-to-end and pin down its AST shape, so
grammar regressions are caught immediately as the language grows.

## CLI

```
cargo run --bin phase -- check examples/radio_pipeline.phase
cargo run --bin phase -- --dump-ast examples/radio_pipeline.phase
cargo run --bin phase -- --dump-tokens examples/radio_pipeline.phase
```

## Next milestone (M2)

Type checker + fixed domain-transition table, still straight-line only:
reject bug-gallery items #1-#4 from the spec (use of a `DeviceOwned`
buffer without `sync`, move/destroy while borrowed, conflicting write
borrows, use-after-destroy).
