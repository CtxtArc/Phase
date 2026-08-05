//! PHASE compiler driver (M1 scope).
//!
//! Usage:
//!   phase check <file>          parse the file, report success/failure
//!   phase --dump-ast <file>     parse and pretty-print the AST
//!   phase --dump-tokens <file>  lex and print the token stream

use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, ExitCode};

fn print_usage() {
    eprintln!("usage:");
    eprintln!("  phase check <file>");
    eprintln!("  phase build <file>          (run from the phase/ project root)");
    eprintln!("  phase --dump-ast <file>");
    eprintln!("  phase --dump-tokens <file>");
    eprintln!("  phase --dump-pir <file>");
}

/// Parse and analyze `src`, printing errors to stderr (prefixed with
/// `path`) and returning `None` on any failure. Shared by every command
/// past `check` that needs a program known to be safe before proceeding.
fn parse_and_analyze(path: &str, src: &str) -> Option<phase_ast::Program> {
    let program = match phase_parser::parse(src) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{path}: {e}");
            return None;
        }
    };
    match phase_analysis::analyze(&program) {
        Ok(()) => Some(program),
        Err(errors) => {
            for e in &errors {
                eprintln!("{path}: error: {e}");
            }
            eprintln!(
                "{path}: {} error{} found",
                errors.len(),
                if errors.len() == 1 { "" } else { "s" }
            );
            None
        }
    }
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
        "--dump-pir" => {
            let Some(program) = parse_and_analyze(path, &src) else {
                return ExitCode::FAILURE;
            };
            match phase_pir::build(&program) {
                Ok(pir) => {
                    println!("{:#?}", pir);
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{path}: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        "build" => {
            let Some(program) = parse_and_analyze(path, &src) else {
                return ExitCode::FAILURE;
            };
            let pir = match phase_pir::build(&program) {
                Ok(pir) => pir,
                Err(e) => {
                    eprintln!("{path}: {e}");
                    return ExitCode::FAILURE;
                }
            };

            let stem = Path::new(path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("out")
                .to_string();

            let generated = phase_codegen_c::generate(&pir, &stem);

            if let Err(e) = fs::create_dir_all("build") {
                eprintln!("error: could not create 'build' directory: {e}");
                return ExitCode::FAILURE;
            }
            let header_path = format!("build/{stem}.gen.h");
            let source_path = format!("build/{stem}.gen.c");
            if let Err(e) = fs::write(&header_path, &generated.header) {
                eprintln!("error: could not write '{header_path}': {e}");
                return ExitCode::FAILURE;
            }
            if let Err(e) = fs::write(&source_path, &generated.source) {
                eprintln!("error: could not write '{source_path}': {e}");
                return ExitCode::FAILURE;
            }

            // runtime/phase_runtime.c is deliberately program-specific demo
            // glue (spec §5.3): it hardcodes `#include "radio_pipeline.gen.h"`
            // and implements exactly that program's `extern fn`s. Only link
            // it when it's actually needed -- a program with no `extern fn`
            // (like examples/mmio_registers.phase) doesn't reference any of
            // its symbols and can build standalone.
            let needs_runtime = !pir.extern_fns.is_empty();
            let runtime_c = "runtime/phase_runtime.c";
            if needs_runtime && !Path::new(runtime_c).exists() {
                eprintln!(
                    "error: '{runtime_c}' not found -- `phase build` must be run from the phase/ project root, and only links against the demo runtime shipped there"
                );
                return ExitCode::FAILURE;
            }

            let binary_path = format!("build/{stem}");
            let mut cc_args = vec![
                "-std=c11".to_string(),
                "-Wall".to_string(),
                "-I".to_string(),
                "build".to_string(),
                "-I".to_string(),
                "runtime".to_string(),
                "-o".to_string(),
                binary_path.clone(),
                source_path.clone(),
            ];
            if needs_runtime {
                cc_args.push(runtime_c.to_string());
            }
            cc_args.push("-lm".to_string());

            let cc_result = Command::new("cc").args(&cc_args).output();

            match cc_result {
                Ok(output) if output.status.success() => {
                    if !output.stderr.is_empty() {
                        eprint!("{}", String::from_utf8_lossy(&output.stderr));
                    }
                    println!("OK: built {binary_path}");
                    ExitCode::SUCCESS
                }
                Ok(output) => {
                    eprintln!("error: cc failed to compile the generated C:");
                    eprint!("{}", String::from_utf8_lossy(&output.stderr));
                    ExitCode::FAILURE
                }
                Err(e) => {
                    eprintln!("error: could not invoke 'cc': {e}");
                    ExitCode::FAILURE
                }
            }
        }
        other => {
            eprintln!("unknown command '{other}'");
            print_usage();
            ExitCode::FAILURE
        }
    }
}
