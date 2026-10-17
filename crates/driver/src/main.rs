//! `phase` -- the PHASE compiler driver.
//!
//! Usage:
//!   phase [options] check <file>          parse + run every safety analysis
//!   phase [options] build <file>          check, lower to C, compile with `cc`
//!   phase [options] --dump-tokens <file>  lex and print the token stream
//!   phase [options] --dump-ast <file>     parse and pretty-print the AST
//!   phase [options] --dump-pir <file>     check and pretty-print the PIR
//!   phase lsp                             language server on stdin/stdout
//!   phase --version | --help

mod diagnostics;
mod json;
mod lsp;

use diagnostics::{Diagnostic, ErrorFormat, Source};
use std::env;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn usage() -> String {
    format!(
        "phase {VERSION} -- physical-domain safety compiler

usage:
  phase [options] check <file>          parse and run every safety analysis
  phase [options] build <file>          check, lower to C, and compile with `cc`
  phase [options] emit-c <file>         check and lower to C only, for another
                                        project's build (docs/embedding.md)
  phase [options] import-svd <file.svd> print a board file for a chip's CMSIS-SVD
                                        register description
  phase [options] --dump-tokens <file>  print the token stream
  phase [options] --dump-ast <file>     print the parsed AST
  phase [options] --dump-pir <file>     check, then print the lowered PIR
  phase lsp                             run the language server on stdin/stdout
  phase --version                       print the version
  phase --help                          print this message

options:
  --error-format=human|short   rustc-style snippets (default), or one
                               `file:line:col: error: message` line each
  --color=auto|always|never    color for human errors (default: auto, which
                               respects NO_COLOR and only colors a terminal)
  --checked                    (build) instrument the program with runtime
                               ownership checks; writes <stem>.checked.gen.c
                               and the binary <stem>-checked
  --stub-externs               (build) generate placeholder bodies for the
                               program's extern fns instead of requiring
                               runtime/<stem>_extern.c; scalar results come
                               from a PRNG seeded by PHASE_STUB_SEED
  --target=TARGET              (build) host (default): compile with `cc` and
                               simulate devices; mps2-an385 / mps2-an505:
                               cross-compile for the ARM MPS2 AN385 board
                               (Cortex-M3) or AN505 board (Cortex-M33, with
                               PL081 DMA controllers) with arm-none-eabi-gcc
                               into <stem>.elf, devices at their real addresses;
                               nucleo-f401re: the ST NUCLEO-F401RE board
                               (STM32F401RE), into <stem>.elf and <stem>.bin
  --run                        (build) run the result: the host binary, the
                               .elf under qemu-system-arm -M <board>, or on
                               the NUCLEO: flash it (probe-rs, openocd or
                               st-flash) and show its serial console
  --out-dir=DIR                (emit-c) where to write <stem>.gen.{{c,h}} and
                               phase_runtime.h (default: next to <file>)
  --only=NAME,NAME,..          (import-svd) just these peripherals

`build` writes <project>/build/<stem>.gen.{{c,h}} and <project>/build/<stem>,
where <project> is the nearest directory at or above <file> that contains a
`runtime/` directory (override with PHASE_RUNTIME_DIR=<dir>)."
    )
}

fn print_usage() {
    eprintln!("{}", usage());
}

/// How diagnostics are printed.
struct Reporter {
    format: ErrorFormat,
    color: bool,
}

impl Reporter {
    fn emit(&self, diag: &Diagnostic, sources: &[Source]) {
        eprint!(
            "{}",
            diagnostics::render(diag, sources, self.format, self.color)
        );
    }

    /// Print every warning and error, and a count when there are errors.
    /// Returns whether the program was rejected.
    fn report(&self, report: &phase_analysis::Report, path: &str, sources: &[Source]) -> bool {
        for e in report.warnings.iter().chain(&report.errors) {
            self.emit(&Diagnostic::from(e), sources);
            if self.format == ErrorFormat::Human {
                eprintln!();
            }
        }
        if report.errors.is_empty() {
            return false;
        }
        let (e, w) = (report.errors.len(), report.warnings.len());
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        if w == 0 {
            eprintln!("{path}: {e} error{} found", plural(e));
        } else {
            eprintln!(
                "{path}: {e} error{} and {w} warning{} found",
                plural(e),
                plural(w)
            );
        }
        true
    }

    fn parse_error(&self, e: &phase_parser::ParseError, path: &str, src: &str) -> ExitCode {
        self.emit(&Diagnostic::from(e), &[Source { path, src }]);
        ExitCode::FAILURE
    }
}

/// The display paths and sources of a loaded program's files.
fn sources_of(files: &[phase_parser::SourceFile]) -> Vec<(String, &str)> {
    files
        .iter()
        .map(|f| (f.path.display().to_string(), f.src.as_str()))
        .collect()
}

fn as_sources<'a>(owned: &'a [(String, &'a str)]) -> Vec<Source<'a>> {
    owned
        .iter()
        .map(|(path, src)| Source { path, src })
        .collect()
}

/// Load `path` and everything it imports, reporting a file that can't be
/// read or parsed.
fn load(path: &str, rep: &Reporter) -> Option<phase_parser::Loaded> {
    match phase_parser::load(Path::new(path), &mut |p| fs::read_to_string(p)) {
        Ok(l) => Some(l),
        Err(e) => {
            let owned = sources_of(&e.files);
            rep.emit(&Diagnostic::from(&e.error), &as_sources(&owned));
            None
        }
    }
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

/// Parse and analyze `src`, reporting any errors, and returning `None` on
/// failure. Shared by every command past `check` that needs a program
/// known to be safe before proceeding.
fn parse_and_analyze(path: &str, rep: &Reporter) -> Option<phase_ast::Program> {
    let loaded = load(path, rep)?;
    let report = phase_analysis::analyze_full(&loaded.program);
    let owned = sources_of(&loaded.files);
    if rep.report(&report, path, &as_sources(&owned)) {
        None
    } else {
        Some(loaded.program)
    }
}

fn main() -> ExitCode {
    let mut format = ErrorFormat::Human;
    let mut color_mode = "auto".to_string();
    let mut checked = false;
    let mut stub_externs = false;
    let mut target = "host".to_string();
    let mut run = false;
    let mut out_dir: Option<PathBuf> = None;
    let mut only: Option<Vec<String>> = None;
    let mut positional: Vec<String> = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(v) = arg.strip_prefix("--error-format=") {
            format = match v {
                "human" => ErrorFormat::Human,
                "short" => ErrorFormat::Short,
                other => {
                    eprintln!("error: unknown --error-format '{other}' (expected human or short)");
                    return ExitCode::FAILURE;
                }
            };
        } else if arg == "--checked" {
            checked = true;
        } else if arg == "--stub-externs" {
            stub_externs = true;
        } else if arg == "--run" {
            run = true;
        } else if let Some(v) = arg.strip_prefix("--out-dir=") {
            out_dir = Some(PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--only=") {
            only = Some(
                v.split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
            );
        } else if arg == "--target" || arg.starts_with("--target=") {
            let v = match arg.strip_prefix("--target=") {
                Some(v) => v.to_string(),
                None => args.next().unwrap_or_default(),
            };
            if v != "host" && Board::named(&v).is_none() {
                eprintln!(
                    "error: unknown --target '{v}' (expected host, mps2-an385, mps2-an505 or nucleo-f401re)"
                );
                return ExitCode::FAILURE;
            }
            target = v;
        } else if let Some(v) = arg.strip_prefix("--color=") {
            if !matches!(v, "auto" | "always" | "never") {
                eprintln!("error: unknown --color '{v}' (expected auto, always, or never)");
                return ExitCode::FAILURE;
            }
            color_mode = v.to_string();
        } else {
            positional.push(arg);
        }
    }

    match positional.first().map(String::as_str) {
        Some("--version" | "-V") if positional.len() == 1 => {
            println!("phase {VERSION}");
            return ExitCode::SUCCESS;
        }
        Some("--help" | "-h" | "help") if positional.len() == 1 => {
            println!("{}", usage());
            return ExitCode::SUCCESS;
        }
        Some("lsp") if positional.len() == 1 => {
            return ExitCode::from(lsp::serve() as u8);
        }
        _ => {}
    }
    if positional.len() != 2 {
        print_usage();
        return ExitCode::FAILURE;
    }

    let color = match color_mode.as_str() {
        "always" => true,
        "never" => false,
        _ => env::var_os("NO_COLOR").is_none() && std::io::stderr().is_terminal(),
    };
    let rep = Reporter { format, color };

    let command = positional[0].as_str();
    let path = &positional[1];

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
            Err(e) => rep.parse_error(&e.into(), path, &src),
        },
        "--dump-ast" => match phase_parser::parse(&src) {
            Ok(program) => {
                println!("{:#?}", program);
                ExitCode::SUCCESS
            }
            Err(e) => rep.parse_error(&e, path, &src),
        },
        "check" => match load(path, &rep) {
            Some(loaded) => {
                let program = &loaded.program;
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

                let report = phase_analysis::analyze_full(program);
                let owned = sources_of(&loaded.files);
                if rep.report(&report, path, &as_sources(&owned)) {
                    return ExitCode::FAILURE;
                }
                let files = match loaded.files.len() {
                    1 => String::new(),
                    n => format!(", {n} files"),
                };
                let warnings = match report.warnings.len() {
                    0 => String::new(),
                    1 => ", 1 warning".to_string(),
                    n => format!(", {n} warnings"),
                };
                println!(
                    "OK: {path} parsed and passed domain/ownership analysis ({entities} entit{}, {fns} fn{}{files}{warnings})",
                    if entities == 1 { "y" } else { "ies" },
                    if fns == 1 { "" } else { "s" },
                );
                ExitCode::SUCCESS
            }
            None => ExitCode::FAILURE,
        },
        "--dump-pir" => {
            let Some(program) = parse_and_analyze(path, &rep) else {
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
        "import-svd" => match phase_svd::import(&src, &phase_svd::Options { only }) {
            Ok(imported) => {
                print!("{}", imported.text);
                for n in &imported.notes {
                    eprintln!("note: {n}");
                }
                eprintln!(
                    "OK: imported {} device{} ({} register declarations) from {path}",
                    imported.devices,
                    if imported.devices == 1 { "" } else { "s" },
                    imported.registers
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {path}: {e}");
                ExitCode::FAILURE
            }
        },
        "emit-c" => {
            let Some(program) = parse_and_analyze(path, &rep) else {
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
            let dir = out_dir.unwrap_or_else(|| {
                Path::new(path)
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("."))
            });
            emit_c(&pir, &stem, &dir)
        }
        "build" => {
            let Some(program) = parse_and_analyze(path, &rep) else {
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

            let generated = if checked {
                phase_codegen_c::generate_checked(&pir, &stem)
            } else {
                phase_codegen_c::generate(&pir, &stem)
            };
            // A checked build gets its own source and binary names so it
            // never silently replaces the normal one.
            let (source_name, binary_name) = if checked {
                (format!("{stem}.checked.gen.c"), format!("{stem}-checked"))
            } else {
                (format!("{stem}.gen.c"), stem.clone())
            };

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
            let source_path = build_dir.join(&source_name).display().to_string();
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
            let needs_extern_runtime = !pir.extern_fns.is_empty();
            if let Some(board) = Board::named(&target) {
                if checked || stub_externs {
                    eprintln!(
                        "error: --checked and --stub-externs need a host build (the checker and \
                         the stubs print and exit through libc); build without --target to use them"
                    );
                    return ExitCode::FAILURE;
                }
                return build_mps2(&BuildMps2 {
                    board,
                    stem: &stem,
                    runtime_dir: &runtime_dir,
                    build_dir: &build_dir,
                    source_path: &source_path,
                    needs_extern_runtime,
                    extern_count: pir.extern_fns.len(),
                    run,
                });
            }

            let generic_runtime_c = runtime_dir.join("phase_runtime.c");
            let extern_runtime_c = if stub_externs {
                let stubs = build_dir.join(format!("{stem}.stubs.c"));
                if let Err(e) = fs::write(&stubs, phase_codegen_c::generate_stubs(&pir, &stem)) {
                    eprintln!("error: could not write '{}': {e}", stubs.display());
                    return ExitCode::FAILURE;
                }
                stubs
            } else {
                runtime_dir.join(format!("{stem}_extern.c"))
            };

            if needs_extern_runtime && !extern_runtime_c.exists() {
                eprintln!(
                    "error: this program declares {} extern fn(s) but '{}' \
                     was not found -- `phase build` needs a hand-written runtime file \
                     implementing them (see runtime/radio_pipeline_extern.c for an example), \
                     or --stub-externs to generate placeholders",
                    pir.extern_fns.len(),
                    extern_runtime_c.display()
                );
                return ExitCode::FAILURE;
            }

            let check_runtime_c = runtime_dir.join("phase_check.c");
            if checked && !check_runtime_c.exists() {
                eprintln!(
                    "error: --checked needs '{}', which was not found",
                    check_runtime_c.display()
                );
                return ExitCode::FAILURE;
            }

            let binary_path = build_dir.join(&binary_name).display().to_string();
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
            if checked {
                cc_args.push(check_runtime_c.display().to_string());
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
                    if run {
                        return run_and_report(Command::new(&binary_path), &binary_path);
                    }
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

/// The runtime header the generated C includes, as this compiler knows it.
const RUNTIME_H: &str = include_str!("../../../runtime/phase_runtime.h");

/// `phase emit-c` (v3.0): the generated C for someone else's build -- a
/// vendor IDE project, a Makefile, CMake. Writes `<stem>.gen.c`,
/// `<stem>.gen.h` and the `phase_runtime.h` they include into `dir`, and
/// nothing else: what to compile them with is docs/embedding.md.
fn emit_c(pir: &phase_pir::PirProgram, stem: &str, dir: &Path) -> ExitCode {
    let generated = phase_codegen_c::generate(pir, stem);
    if let Err(e) = fs::create_dir_all(dir) {
        eprintln!("error: could not create '{}': {e}", dir.display());
        return ExitCode::FAILURE;
    }
    let files = [
        (format!("{stem}.gen.h"), generated.header.as_str()),
        (format!("{stem}.gen.c"), generated.source.as_str()),
        ("phase_runtime.h".to_string(), RUNTIME_H),
    ];
    let mut written = Vec::new();
    for (name, text) in files {
        let p = dir.join(name);
        if let Err(e) = fs::write(&p, text) {
            eprintln!("error: could not write '{}': {e}", p.display());
            return ExitCode::FAILURE;
        }
        written.push(p.display().to_string());
    }
    println!("OK: wrote {}", written.join(", "));
    ExitCode::SUCCESS
}

/// The line the NUCLEO runtime prints when `main` returns
/// (runtime/hw/phase_runtime_hw.c).
const BOARD_DONE: &str = "[phase] main returned";

/// The end of a build for a board with an ST-LINK (v3.0): a `.bin` next to
/// the `.elf` (what the board's USB drive takes), and with `--run`, flash
/// it and show its console until `main` returns.
fn finish_stlink_build(
    cc: &str,
    elf: &Path,
    board: &str,
    chip: &str,
    openocd_target: &str,
    run: bool,
) -> ExitCode {
    let objcopy = env::var("PHASE_ARM_OBJCOPY").unwrap_or_else(|_| match cc.strip_suffix("gcc") {
        Some(prefix) => format!("{prefix}objcopy"),
        None => "arm-none-eabi-objcopy".to_string(),
    });
    let bin = elf.with_extension("bin");
    match Command::new(&objcopy)
        .args(["-O", "binary"])
        .arg(elf)
        .arg(&bin)
        .output()
    {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            eprintln!("error: {objcopy} failed:");
            eprint!("{}", String::from_utf8_lossy(&out.stderr));
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("error: could not invoke '{objcopy}' ({e}); set PHASE_ARM_OBJCOPY");
            return ExitCode::FAILURE;
        }
    }
    let (elf_s, bin_s) = (elf.display().to_string(), bin.display().to_string());
    println!("OK: built {elf_s} and {bin_s} for {board}");
    if !run {
        println!("    flash it: copy {bin_s} to the board's USB drive (NOD_F401RE),");
        println!("              or: probe-rs download --chip {chip} {elf_s} && probe-rs reset --chip {chip}");
        println!("    its console: the ST-LINK's serial port at 115200 baud (e.g. picocom -b 115200 /dev/ttyACM0)");
        println!("    or let `phase build --target={board} --run` do both");
        return ExitCode::SUCCESS;
    }

    // Listen before flashing: the program starts as soon as it is written.
    let serial = env::var_os("PHASE_SERIAL")
        .map(PathBuf::from)
        .or_else(find_stlink_serial);
    let lines = match &serial {
        Some(dev) => match open_serial(dev) {
            Ok(rx) => Some(rx),
            Err(e) => {
                eprintln!(
                    "error: could not open the board's serial port '{}': {e}",
                    dev.display()
                );
                return ExitCode::FAILURE;
            }
        },
        None => {
            eprintln!(
                "warning: no ST-LINK serial port found (set PHASE_SERIAL=/dev/ttyACM0 or similar); \
                 flashing without showing the console"
            );
            None
        }
    };
    if let Err(e) = flash_stlink(elf, &bin, chip, openocd_target) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    let Some(rx) = lines else {
        return ExitCode::SUCCESS;
    };
    let timeout = env::var("PHASE_RUN_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30u64);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let mut seen = String::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match rx.recv_timeout(left) {
            Ok(chunk) => {
                let text: String = String::from_utf8_lossy(&chunk).replace('\r', "");
                print!("{text}");
                use std::io::Write;
                let _ = std::io::stdout().flush();
                seen.push_str(&text);
                if seen.contains(BOARD_DONE) {
                    return ExitCode::SUCCESS;
                }
            }
            Err(_) => {
                eprintln!(
                    "\nerror: '{BOARD_DONE}' did not appear within {timeout} s \
                     (PHASE_RUN_TIMEOUT sets the limit)"
                );
                return ExitCode::FAILURE;
            }
        }
    }
}

/// The ST-LINK's virtual COM port, if one is plugged in.
fn find_stlink_serial() -> Option<PathBuf> {
    if let Ok(dir) = fs::read_dir("/dev/serial/by-id") {
        let mut found: Vec<PathBuf> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p.to_string_lossy().to_lowercase();
                n.contains("stlink") || n.contains("st-link")
            })
            .collect();
        found.sort();
        if let Some(p) = found.into_iter().next() {
            return Some(p);
        }
    }
    if let Ok(dir) = fs::read_dir("/dev") {
        let mut found: Vec<PathBuf> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                let n = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                n.starts_with("cu.usbmodem")
            })
            .collect();
        found.sort();
        if let Some(p) = found.into_iter().next() {
            return Some(p);
        }
    }
    let acm = PathBuf::from("/dev/ttyACM0");
    acm.exists().then_some(acm)
}

/// Set `dev` to 115200 baud, raw, and read it on a thread.
fn open_serial(dev: &Path) -> Result<std::sync::mpsc::Receiver<Vec<u8>>, String> {
    let flag = if cfg!(target_os = "macos") {
        "-f"
    } else {
        "-F"
    };
    let st = Command::new("stty")
        .arg(flag)
        .arg(dev)
        .args(["115200", "raw", "-echo"])
        .status()
        .map_err(|e| format!("could not run stty: {e}"))?;
    if !st.success() {
        return Err("stty could not configure it".to_string());
    }
    let mut f = fs::File::open(dev).map_err(|e| e.to_string())?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 256];
        while let Ok(n) = f.read(&mut buf) {
            if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    Ok(rx)
}

/// Is `tool` on PATH?
fn on_path(tool: &str) -> bool {
    env::var_os("PATH")
        .is_some_and(|paths| env::split_paths(&paths).any(|d| d.join(tool).is_file()))
}

/// Write the program to the board and start it, with the first of
/// probe-rs, OpenOCD and st-flash that is installed.
fn flash_stlink(elf: &Path, bin: &Path, chip: &str, openocd_target: &str) -> Result<(), String> {
    let run = |mut cmd: Command, what: &str| -> Result<(), String> {
        println!("flashing with {what}");
        let out = cmd
            .output()
            .map_err(|e| format!("could not run {what}: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{what} failed:\n{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ))
        }
    };
    if on_path("probe-rs") {
        let mut dl = Command::new("probe-rs");
        dl.args(["download", "--chip", chip]).arg(elf);
        run(dl, "probe-rs")?;
        let mut reset = Command::new("probe-rs");
        reset.args(["reset", "--chip", chip]);
        return run(reset, "probe-rs reset");
    }
    if on_path("openocd") {
        let mut cmd = Command::new("openocd");
        cmd.args(["-f", "interface/stlink.cfg", "-f", openocd_target, "-c"])
            .arg(format!("program {} verify reset exit", elf.display()));
        return run(cmd, "openocd");
    }
    if on_path("st-flash") {
        let mut cmd = Command::new("st-flash");
        cmd.args(["--reset", "write"]).arg(bin).arg("0x08000000");
        return run(cmd, "st-flash");
    }
    Err(format!(
        "--run needs probe-rs, openocd or st-flash on PATH to flash the board \
         (or copy {} to its USB drive)",
        bin.display()
    ))
}

/// Run a built program with inherited stdio, and exit with its status.
fn run_and_report(mut cmd: Command, what: &str) -> ExitCode {
    match cmd.status() {
        Ok(st) if st.success() => ExitCode::SUCCESS,
        Ok(st) => {
            eprintln!("error: '{what}' exited with {st}");
            ExitCode::from(st.code().map_or(1, |c| c.clamp(1, 255) as u8))
        }
        Err(e) => {
            eprintln!("error: could not run '{what}': {e}");
            ExitCode::FAILURE
        }
    }
}

/// A bare-metal target board: how to compile for it and run it on QEMU.
#[derive(Clone, Copy)]
struct Board {
    /// `--target=` name.
    name: &'static str,
    cpu: &'static str,
    /// More compiler flags for the CPU.
    cflags: &'static [&'static str],
    linker_script: &'static str,
    /// Defined for the C (the runtime picks the board's addresses by it).
    define: &'static str,
    /// How `--run` runs it: on QEMU's model of the board, or on the board.
    runs_on: RunsOn,
}

#[derive(Clone, Copy)]
enum RunsOn {
    /// `qemu-system-arm -M <machine>`.
    Qemu(&'static str),
    /// The board itself, through its ST-LINK: flashed with `probe-rs`
    /// (`--chip <chip>`), OpenOCD or `st-flash`, and its console read
    /// from the ST-LINK's serial port.
    StLink {
        chip: &'static str,
        openocd_target: &'static str,
    },
}

impl Board {
    const ALL: [Board; 3] = [
        Board {
            name: "mps2-an385",
            cpu: "cortex-m3",
            cflags: &[],
            linker_script: "mps2an385.ld",
            define: "PHASE_BOARD_MPS2_AN385",
            runs_on: RunsOn::Qemu("mps2-an385"),
        },
        Board {
            name: "mps2-an505",
            cpu: "cortex-m33",
            cflags: &[],
            linker_script: "mps2an505.ld",
            define: "PHASE_BOARD_MPS2_AN505",
            runs_on: RunsOn::Qemu("mps2-an505"),
        },
        Board {
            name: "nucleo-f401re",
            cpu: "cortex-m4",
            // The FPU is off at reset; soft float needs nothing turned on.
            cflags: &["-mfloat-abi=soft"],
            linker_script: "nucleo_f401re.ld",
            define: "PHASE_BOARD_NUCLEO_F401RE",
            runs_on: RunsOn::StLink {
                chip: "STM32F401RETx",
                openocd_target: "target/stm32f4x.cfg",
            },
        },
    ];

    fn named(name: &str) -> Option<Board> {
        Self::ALL.into_iter().find(|b| b.name == name)
    }
}

struct BuildMps2<'a> {
    board: Board,
    stem: &'a str,
    runtime_dir: &'a Path,
    build_dir: &'a Path,
    source_path: &'a str,
    needs_extern_runtime: bool,
    extern_count: usize,
    run: bool,
}

/// `--target=mps2-an385` / `mps2-an505`: cross-compile the generated C with
/// the bare-metal runtime in runtime/hw/ (vector table and startup, linker
/// script, real-hardware `sync` waits) for an ARM MPS2 board, and
/// optionally run it on QEMU's model of that board. `PHASE_TARGET_HW` puts
/// every `device` at its declared address.
fn build_mps2(b: &BuildMps2) -> ExitCode {
    let board = b.board;
    let hw = b.runtime_dir.join("hw");
    let needed = [
        "startup.c",
        "phase_runtime_hw.c",
        "libc_min.c",
        board.linker_script,
    ];
    if let Some(missing) = needed.iter().find(|f| !hw.join(f).is_file()) {
        eprintln!(
            "error: --target={} needs '{}', which was not found",
            board.name,
            hw.join(missing).display()
        );
        return ExitCode::FAILURE;
    }
    // A target-specific extern runtime wins over the host one.
    let extern_c = [
        hw.join(format!("{}_extern.c", b.stem)),
        b.runtime_dir.join(format!("{}_extern.c", b.stem)),
    ]
    .into_iter()
    .find(|p| p.is_file());
    if b.needs_extern_runtime && extern_c.is_none() {
        eprintln!(
            "error: this program declares {} extern fn(s) but neither '{}' nor '{}' was found -- \
             a bare-metal build needs freestanding implementations of them",
            b.extern_count,
            hw.join(format!("{}_extern.c", b.stem)).display(),
            b.runtime_dir.join(format!("{}_extern.c", b.stem)).display()
        );
        return ExitCode::FAILURE;
    }
    let cc = env::var("PHASE_ARM_CC").unwrap_or_else(|_| "arm-none-eabi-gcc".to_string());
    let elf = b.build_dir.join(format!("{}.elf", b.stem));
    let mut cmd = Command::new(&cc);
    cmd.arg(format!("-mcpu={}", board.cpu));
    cmd.args(board.cflags);
    cmd.arg(format!("-D{}=1", board.define));
    cmd.args([
        "-mthumb",
        "-ffreestanding",
        "-nostdlib",
        "-nostartfiles",
        "-O1",
        "-std=c11",
        "-Wall",
        "-DPHASE_TARGET_HW=1",
    ])
    .arg("-T")
    .arg(hw.join(board.linker_script))
    .arg("-I")
    .arg(b.build_dir)
    .arg("-I")
    .arg(b.runtime_dir)
    .arg("-I")
    .arg(&hw)
    .arg("-o")
    .arg(&elf)
    .arg(hw.join("startup.c"))
    .arg(hw.join("phase_runtime_hw.c"))
    .arg(hw.join("libc_min.c"))
    .arg(b.source_path);
    if b.needs_extern_runtime {
        cmd.args(extern_c.iter());
    }
    cmd.arg("-lgcc");
    match cmd.output() {
        Ok(out) if out.status.success() => {
            if !out.stderr.is_empty() {
                eprint!("{}", String::from_utf8_lossy(&out.stderr));
            }
        }
        Ok(out) => {
            eprintln!("error: {cc} failed to compile the generated C:");
            eprint!("{}", String::from_utf8_lossy(&out.stderr));
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!(
                "error: could not invoke '{cc}' ({e}); --target={} needs the GNU Arm \
                 Embedded toolchain on PATH (or PHASE_ARM_CC set to it)",
                board.name
            );
            return ExitCode::FAILURE;
        }
    }
    let elf_s = elf.display().to_string();
    let machine = match board.runs_on {
        RunsOn::Qemu(m) => m,
        RunsOn::StLink {
            chip,
            openocd_target,
        } => {
            return finish_stlink_build(&cc, &elf, board.name, chip, openocd_target, b.run);
        }
    };
    let qemu_args = [
        "-M",
        machine,
        "-nographic",
        "-monitor",
        "none",
        "-semihosting-config",
        "enable=on,target=native",
        "-kernel",
    ];
    if !b.run {
        println!("OK: built {elf_s} for {}", board.name);
        println!(
            "    run it: qemu-system-arm {} {elf_s}",
            qemu_args.join(" ")
        );
        return ExitCode::SUCCESS;
    }
    println!(
        "OK: built {elf_s} for {}; running it on qemu-system-arm",
        board.name
    );
    let qemu = env::var("PHASE_QEMU").unwrap_or_else(|_| "qemu-system-arm".to_string());
    let mut cmd = Command::new(&qemu);
    cmd.args(qemu_args).arg(&elf);
    run_and_report(cmd, &qemu)
}
