//! Integration tests for M1: every example under `examples/` must lex and
//! parse successfully, and the happy-path pipeline's AST shape is pinned
//! down explicitly so accidental grammar regressions are caught.

use phase_ast::{Item, Stmt};
use std::fs;
use std::path::PathBuf;

fn workspace_root() -> PathBuf {
    // crates/driver -> crates -> workspace root
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read_example(name: &str) -> String {
    let path = workspace_root().join("examples").join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {path:?}: {e}"))
}

#[test]
fn radio_pipeline_lexes_successfully() {
    let src = read_example("radio_pipeline.phase");
    let tokens = phase_lexer::tokenize(&src);
    assert!(tokens.is_ok(), "lexing failed: {:?}", tokens.err());
}

#[test]
fn radio_pipeline_parses_successfully() {
    let src = read_example("radio_pipeline.phase");
    let program = phase_parser::parse(&src);
    assert!(program.is_ok(), "parsing failed: {:?}", program.err());
}

#[test]
fn radio_pipeline_has_expected_top_level_shape() {
    let src = read_example("radio_pipeline.phase");
    let program = phase_parser::parse(&src).unwrap();

    let entity_count = program
        .items
        .iter()
        .filter(|i| matches!(i, Item::Entity(_)))
        .count();
    let state_count = program
        .items
        .iter()
        .filter(|i| matches!(i, Item::State(_)))
        .count();
    let extern_fn_count = program
        .items
        .iter()
        .filter(|i| matches!(i, Item::ExternFn(_)))
        .count();
    let fn_count = program
        .items
        .iter()
        .filter(|i| matches!(i, Item::Fn(_)))
        .count();

    assert_eq!(entity_count, 1, "expected exactly one `entity` decl");
    assert_eq!(state_count, 1, "expected exactly one `state` decl");
    assert_eq!(extern_fn_count, 3, "expected three `extern fn` decls");
    assert_eq!(fn_count, 1, "expected exactly one `fn` decl (main)");
}

#[test]
fn radio_pipeline_main_body_statement_sequence() {
    let src = read_example("radio_pipeline.phase");
    let program = phase_parser::parse(&src).unwrap();

    let main_fn = program
        .items
        .iter()
        .find_map(|i| match i {
            Item::Fn(f) if f.name == "main" => Some(f),
            _ => None,
        })
        .expect("main() not found");

    // Pin down the statement *kinds* in order — this is the load-bearing
    // shape the domain/ownership analyzer (M2) will walk over next.
    let kinds: Vec<&'static str> = main_fn
        .body
        .stmts
        .iter()
        .map(|s| match s {
            Stmt::VarDecl { .. } => "VarDecl",
            Stmt::Move { .. } => "Move",
            Stmt::Sync { .. } => "Sync",
            Stmt::Release { .. } => "Release",
            Stmt::Destroy { .. } => "Destroy",
            Stmt::Expr(_) => "Expr",
            Stmt::Return(_) => "Return",
        })
        .collect();

    assert_eq!(
        kinds,
        vec![
            "VarDecl", // buffer<Sample, 1024> raw @DMA;
            "Expr",    // device_capture(raw);
            "Sync",    // sync(raw);
            "Move",    // move raw -> @RAM;
            "VarDecl", // buffer<Sample, 1024> filtered @RAM;
            "Expr",    // fir_filter(raw, filtered);
            "Move",    // move filtered -> @DEVICE;
            "Expr",    // device_playback(filtered);
            "Sync",    // sync(filtered);
        ]
    );
}

/// A handful of inline fixtures covering constructs the single example file
/// doesn't otherwise exercise together. These are round-tripped through the
/// full lex -> parse pipeline as smoke tests; semantic rejection of the
/// *invalid* patterns (bug gallery) is the job of the M2+ analyzer, not the
/// parser — the parser's only job is to accept well-formed syntax.
mod syntax_smoke_tests {
    #[test]
    fn empty_program_parses() {
        assert!(phase_parser::parse("").is_ok());
    }

    #[test]
    fn entity_with_multiple_fields() {
        let src = "entity Packet : @RAM { seq: i32, payload: buffer<u8, 256> }";
        assert!(phase_parser::parse(src).is_ok());
    }

    #[test]
    fn fn_with_return_type_and_return_stmt() {
        let src = "fn make() -> Sample { return Sample { value: 1.0 }; }";
        assert!(phase_parser::parse(src).is_ok());
    }

    #[test]
    fn destroy_statement() {
        let src = "fn main() { let x = 1; destroy x; }";
        assert!(phase_parser::parse(src).is_ok());
    }

    #[test]
    fn mmio_round_trip() {
        let src = r#"
            fn main() {
                let s: u32 = volatile_read(UART_STATUS @MMIO);
                volatile_write(UART_CONTROL @MMIO, s);
            }
        "#;
        assert!(phase_parser::parse(src).is_ok());
    }

    #[test]
    fn syntax_error_reports_a_span() {
        let err = phase_parser::parse("fn main() { let x = ; }").unwrap_err();
        assert!(err.span.line >= 1);
    }
}
