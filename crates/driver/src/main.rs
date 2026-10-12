//! `phase` -- the PHASE compiler driver.
//!
//! Usage:
//!   phase check <file>          parse + run every safety analysis
//!   phase build <file>          check, lower to C, compile with `cc`
//!   phase --dump-tokens <file>  lex and print the token stream
//!   phase --dump-ast <file>     parse and pretty-print the AST
//!   phase --dump-pir <file>     check and pretty-print the PIR
//!   phase --version | --help

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn usage() -> String {
    format!(
        "phase {VERSION} -- physical-domain safety compiler

usage:
  phase check <file>          parse and run every safety analysis
  phase build <file>          check, lower to C, and compile with `cc`
  phase --dump-tokens <file>  print the token stream
  phase --dump-ast <file>     print the parsed AST
  phase --dump-pir <file>     check, then print the lowered PIR
  phase --version             print the version
  phase --help                print this message

`build` writes <project>/build/<stem>.gen.{{c,h}} and <project>/build/<stem>,
where <project> is the nearest directory at or above <file> that contains a
`runtime/` directory (override with PHASE_RUNTIME_DIR=<dir>)."
    )
}

fn print_usage() {
    eprintln!("{}", usage());
}

/// Locate the runtime directory for `source`: `$PHASE_RUNTIME_DIR` if set,
/// otherwise the nearest `runtime/` at or above the source file, otherwise
/// the nearest one at or above the current directory.
fn find_runtime_dir(source: &Path) -> Option<PathBuf> {
    if let Some(dir) = env::var_os("PHASE_RUNTIME_DIR") {
        return Some(PathBuf::from(dir));
    }
    let search_from = |start: PathBuf| {
        start
            .ancestors()
            .map(|a| a.join("runtime"))
            .find(|r| r.join("phase_runtime.h").is_file())
    };
    let source_dir = fs::canonicalize(source)
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    source_dir
        .and_then(search_from)
        .or_else(|| env::current_dir().ok().and_then(search_from))
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
    match args.get(1).map(String::as_str) {
        Some("--version" | "-V") if args.len() == 2 => {
            println!("phase {VERSION}");
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h" | "help") if args.len() == 2 => {
            println!("{}", usage());
            return ExitCode::SUCCESS;
        }
        _ => {}
    }
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

            let Some(runtime_dir) = find_runtime_dir(Path::new(path)) else {
                eprintln!(
                    "error: could not find a PHASE `runtime/` directory (containing \
                     phase_runtime.h) at or above '{path}' or the current directory; \
                     set PHASE_RUNTIME_DIR to point at one"
                );
                return ExitCode::FAILURE;
            };
            let project_dir = runtime_dir
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            let build_dir = project_dir.join("build");

            if let Err(e) = fs::create_dir_all(&build_dir) {
                eprintln!("error: could not create '{}': {e}", build_dir.display());
                return ExitCode::FAILURE;
            }
            let header_path = build_dir
                .join(format!("{stem}.gen.h"))
                .display()
                .to_string();
            let source_path = build_dir
                .join(format!("{stem}.gen.c"))
                .display()
                .to_string();
            if let Err(e) = fs::write(&header_path, &generated.header) {
                eprintln!("error: could not write '{header_path}': {e}");
                return ExitCode::FAILURE;
            }
            if let Err(e) = fs::write(&source_path, &generated.source) {
                eprintln!("error: could not write '{source_path}': {e}");
                return ExitCode::FAILURE;
            }

            // Two kinds of runtime files (spec §5.3):
            //   - runtime/phase_runtime.c is generic (sim_dma_wait/
            //     sim_device_wait only) and never depends on any one
            //     program's types, so it's always linked when it exists.
            //   - runtime/<stem>_extern.c is hand-written, program-specific
            //     glue implementing exactly this program's `extern fn`s
            //     (the compiler only ever generates prototypes for those,
            //     never bodies -- spec §3.9). Only linked if the program
            //     actually declares any `extern fn`, and only if a
            //     matching file exists.
            let generic_runtime_c = runtime_dir.join("phase_runtime.c");
            let extern_runtime_c = runtime_dir.join(format!("{stem}_extern.c"));
            let needs_extern_runtime = !pir.extern_fns.is_empty();

            if needs_extern_runtime && !extern_runtime_c.exists() {
                eprintln!(
                    "error: this program declares {} extern fn(s) but '{}' \
                     was not found -- `phase build` needs a hand-written runtime file \
                     implementing them (see runtime/radio_pipeline_extern.c for an example)",
                    pir.extern_fns.len(),
                    extern_runtime_c.display()
                );
                return ExitCode::FAILURE;
            }

            let binary_path = build_dir.join(&stem).display().to_string();
            let mut cc_args = vec![
                "-std=c11".to_string(),
                "-Wall".to_string(),
                "-I".to_string(),
                build_dir.display().to_string(),
                "-I".to_string(),
                runtime_dir.display().to_string(),
                "-o".to_string(),
                binary_path.clone(),
                source_path.clone(),
            ];
            if generic_runtime_c.exists() {
                cc_args.push(generic_runtime_c.display().to_string());
            }
            if needs_extern_runtime {
                cc_args.push(extern_runtime_c.display().to_string());
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
