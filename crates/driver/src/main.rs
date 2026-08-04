//! PHASE compiler driver (M1 scope).
//!
//! Usage:
//!   phase check <file>          parse the file, report success/failure
//!   phase --dump-ast <file>     parse and pretty-print the AST
//!   phase --dump-tokens <file>  lex and print the token stream

use std::env;
use std::fs;
use std::process::ExitCode;

fn print_usage() {
    eprintln!("usage:");
    eprintln!("  phase check <file>");
    eprintln!("  phase --dump-ast <file>");
    eprintln!("  phase --dump-tokens <file>");
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        print_usage();
        return ExitCode::FAILURE;
    }

    let command = args[1].as_str();
    let path = &args[2];

    let src = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: could not read '{path}': {e}");
            return ExitCode::FAILURE;
        }
    };

    match command {
        "--dump-tokens" => match phase_lexer::tokenize(&src) {
            Ok(tokens) => {
                for t in tokens {
                    println!("{:>4}:{:<4} {:?}", t.span.line, t.span.col, t.kind);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        "--dump-ast" => match phase_parser::parse(&src) {
            Ok(program) => {
                println!("{:#?}", program);
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{e}");
                ExitCode::FAILURE
            }
        },
        "check" => match phase_parser::parse(&src) {
            Ok(program) => {
                let entities = program
                    .items
                    .iter()
                    .filter(|i| matches!(i, phase_ast::Item::Entity(_)))
                    .count();
                let fns = program
                    .items
                    .iter()
                    .filter(|i| matches!(i, phase_ast::Item::Fn(_)))
                    .count();

                match phase_analysis::analyze(&program) {
                    Ok(()) => {
                        println!(
                            "OK: {path} parsed and passed domain/ownership analysis ({entities} entit{}, {fns} fn{})",
                            if entities == 1 { "y" } else { "ies" },
                            if fns == 1 { "" } else { "s" },
                        );
                        ExitCode::SUCCESS
                    }
                    Err(errors) => {
                        for e in &errors {
                            eprintln!("{path}: error: {e}");
                        }
                        eprintln!(
                            "{path}: {} error{} found",
                            errors.len(),
                            if errors.len() == 1 { "" } else { "s" }
                        );
                        ExitCode::FAILURE
                    }
                }
            }
            Err(e) => {
                eprintln!("{path}: {e}");
                ExitCode::FAILURE
            }
        },
        other => {
            eprintln!("unknown command '{other}'");
            print_usage();
            ExitCode::FAILURE
        }
    }
}
