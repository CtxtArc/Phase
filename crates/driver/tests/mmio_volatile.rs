//! M6's actual claim (bug-gallery #7) is that `volatile_read`/
//! `volatile_write` accesses are never reordered or eliminated by codegen.
//! Checking the *textual C* (see phase_codegen_c's unit tests) is a good
//! first check, but the real-world failure mode this guards against is an
//! *optimizing compiler* deciding a store is dead and removing it. This
//! test proves the guarantee survives real optimization: it compiles
//! examples/mmio_registers.phase at `-O2` to assembly and confirms both
//! back-to-back writes to the same register are still present as separate
//! instructions -- if `volatile` weren't correctly applied, a competent
//! optimizer (GCC, here) would collapse
//!   UART_CONTROL = 1;
//!   UART_CONTROL = 2;
//! down to just the second store, since the first is otherwise dead.
//!
//! This test's assembly-scanning is inherently platform/toolchain-specific
//! (x86_64 + GCC's AT&T syntax, as installed in this project's dev
//! environment) -- that's an intrinsic property of testing at the
//! assembly level, not something to work around.

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Lines that actually reference `symbol` as an instruction operand,
/// excluding assembler directives (`.comm`, `.globl`, ...) and label
/// definitions (`symbol:`) -- i.e. real, executed accesses.
fn instruction_references(asm: &str, symbol: &str) -> usize {
    asm.lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.contains(symbol) && !trimmed.starts_with('.') && !trimmed.ends_with(':')
        })
        .count()
}

#[test]
fn volatile_writes_survive_dead_store_elimination_at_o2() {
    let root = workspace_root();
    let src_path = root.join("examples/mmio_registers.phase");
    let src = std::fs::read_to_string(&src_path).expect("mmio_registers.phase must exist");

    let program = phase_parser::parse(&src).expect("must parse");
    phase_analysis::analyze(&program).expect("must pass analysis");
    let pir = phase_pir::build(&program).expect("must lower to PIR");
    assert!(
        pir.extern_fns.is_empty(),
        "this example is expected to need no extern fns / no runtime linkage"
    );
    let generated = phase_codegen_c::generate(&pir, "mmio_registers");

    let build_dir = root.join("build");
    std::fs::create_dir_all(&build_dir).unwrap();
    let header_path = build_dir.join("mmio_registers.gen.h");
    let source_path = build_dir.join("mmio_registers.gen.c");
    std::fs::write(&header_path, &generated.header).unwrap();
    std::fs::write(&source_path, &generated.source).unwrap();

    let asm_path = build_dir.join("mmio_registers.s");
    let cc_output = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-O2", "-S", "-I"])
        .arg(&build_dir)
        .args(["-I"])
        .arg(root.join("runtime"))
        .args(["-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("failed to invoke cc");

    assert!(
        cc_output.status.success(),
        "cc -O2 -S failed:\n{}",
        String::from_utf8_lossy(&cc_output.stderr)
    );
    assert!(
        cc_output.stderr.is_empty(),
        "cc produced warnings at -O2 (expected zero):\n{}",
        String::from_utf8_lossy(&cc_output.stderr)
    );

    let asm = std::fs::read_to_string(&asm_path).expect("failed to read generated assembly");

    // Two writes to UART_CONTROL in the source must survive as two
    // separate instructions -- if volatile weren't applied, -O2 would
    // eliminate the first as a dead store, leaving only one.
    let control_refs = instruction_references(&asm, "UART_CONTROL");
    assert!(
        control_refs >= 2,
        "expected at least 2 real instruction-level references to UART_CONTROL \
         at -O2 (one per volatile_write in the source), found {control_refs}. \
         This would mean dead-store elimination collapsed the two writes --\n\
         exactly the bug volatile is supposed to prevent.\n\nAssembly:\n{asm}"
    );

    // The read and the other two registers must also actually appear --
    // nothing here should be optimized away entirely (e.g. because the
    // program's result is otherwise unused).
    assert!(
        instruction_references(&asm, "UART_STATUS") >= 1,
        "expected UART_STATUS to still be read at -O2"
    );
    assert!(
        instruction_references(&asm, "UART_DATA") >= 1,
        "expected UART_DATA to still be written at -O2"
    );
}

#[test]
fn mmio_example_builds_and_runs_standalone_without_the_demo_runtime() {
    // examples/mmio_registers.phase declares no `extern fn`, so it should
    // build and link without runtime/phase_runtime.c at all (see the
    // `needs_runtime` logic in the driver's `build` command).
    let root = workspace_root();
    let src_path = root.join("examples/mmio_registers.phase");
    let src = std::fs::read_to_string(&src_path).expect("mmio_registers.phase must exist");

    let program = phase_parser::parse(&src).expect("must parse");
    phase_analysis::analyze(&program).expect("must pass analysis");
    let pir = phase_pir::build(&program).expect("must lower to PIR");
    let generated = phase_codegen_c::generate(&pir, "mmio_registers");

    let build_dir = root.join("build");
    std::fs::create_dir_all(&build_dir).unwrap();
    std::fs::write(build_dir.join("mmio_registers.gen.h"), &generated.header).unwrap();
    std::fs::write(build_dir.join("mmio_registers.gen.c"), &generated.source).unwrap();

    let binary_path = build_dir.join("mmio_registers_standalone");
    let cc_output = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-I"])
        .arg(&build_dir)
        .args(["-I"])
        .arg(root.join("runtime"))
        .args(["-o"])
        .arg(&binary_path)
        .arg(build_dir.join("mmio_registers.gen.c"))
        // deliberately NOT linking runtime/phase_runtime.c
        .output()
        .expect("failed to invoke cc");

    assert!(
        cc_output.status.success(),
        "standalone compile+link failed:\n{}",
        String::from_utf8_lossy(&cc_output.stderr)
    );

    let run_output = Command::new(&binary_path)
        .output()
        .expect("failed to run the compiled binary");
    assert!(run_output.status.success());
}
