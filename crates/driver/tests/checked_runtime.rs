//! The runtime-checked build (`phase build --checked`, runtime/phase_check.c).
//!
//! Two directions:
//! - every buildable example runs clean when checked;
//! - programs that violate the ownership rules -- compiled here *without*
//!   the static analyzer, straight from parse to checked C -- are caught at
//!   run time with exit status 70 and a precise message. That shows the
//!   runtime checker is a real, independent oracle, which is what the
//!   fuzzer (crates/fuzz) relies on.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

/// Compile `src` as a checked build with stub externs, skipping the static
/// analyzer, and run it. Returns (exit code, stderr).
fn run_unanalyzed(name: &str, src: &str, seed: u32) -> (i32, String) {
    let root = workspace_root();
    let dir = root.join("build").join("checked_runtime_tests").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    let program = phase_parser::parse(src).expect("fixture must parse");
    let pir = phase_pir::build(&program).expect("fixture must lower");
    let generated = phase_codegen_c::generate_checked(&pir, name);
    std::fs::write(dir.join(format!("{name}.gen.h")), &generated.header).unwrap();
    std::fs::write(dir.join(format!("{name}.gen.c")), &generated.source).unwrap();
    std::fs::write(
        dir.join(format!("{name}.stubs.c")),
        phase_codegen_c::generate_stubs(&pir, name),
    )
    .unwrap();
    let binary = dir.join(name);
    compile(&root, &dir, name, &binary);
    let out = Command::new(&binary)
        .env("PHASE_STUB_SEED", seed.to_string())
        .output()
        .unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn compile(root: &Path, dir: &Path, name: &str, binary: &Path) {
    let runtime = root.join("runtime");
    let out = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Wno-unused-variable", "-I"])
        .arg(dir)
        .arg("-I")
        .arg(&runtime)
        .arg("-o")
        .arg(binary)
        .arg(dir.join(format!("{name}.gen.c")))
        .arg(dir.join(format!("{name}.stubs.c")))
        .arg(runtime.join("phase_runtime.c"))
        .arg(runtime.join("phase_check.c"))
        .arg("-lm")
        .output()
        .expect("cc must be available");
    assert!(
        out.status.success(),
        "cc failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[track_caller]
fn caught(name: &str, src: &str, needle: &str) {
    let (code, stderr) = run_unanalyzed(name, src, 1);
    assert_eq!(
        code, 70,
        "expected a runtime check failure, got exit {code}; stderr:\n{stderr}"
    );
    assert!(
        stderr.contains(&format!("PHASE runtime check failed: {needle}")),
        "expected {needle:?} in:\n{stderr}"
    );
}

const SIGS: &str = r#"
    extern fn look(x: read buffer<u8, 8> @RAM);
    extern fn poke(x: write buffer<u8, 8> @RAM);
    extern fn fir(a: read buffer<u8, 8> @RAM, b: write buffer<u8, 8> @RAM);
    extern fn start(x: write buffer<u8, 8> @RAM -> @DMA);
"#;

fn with(body: &str) -> String {
    format!("{SIGS}\n{body}")
}

#[test]
fn use_before_sync_is_caught() {
    caught(
        "rt_use_before_sync",
        &with("fn main() { buffer<u8, 8> x @RAM; start(x); look(x); sync(x); }"),
        "'x' is in @DMA but 'look' expects @RAM",
    );
}

#[test]
fn destroy_while_dma_is_caught() {
    caught(
        "rt_destroy_dma",
        &with("fn main() { buffer<u8, 8> x @RAM; start(x); destroy x; }"),
        "'x' destroyed while owned by @DMA",
    );
}

#[test]
fn leaving_scope_while_dma_owned_is_caught() {
    caught(
        "rt_scope_leak",
        &with("fn main() { buffer<u8, 8> x @RAM; start(x); }"),
        "'x' goes out of scope while owned by @DMA",
    );
}

#[test]
fn use_after_release_is_caught() {
    caught(
        "rt_after_release",
        &with("fn main() { buffer<u8, 8> x @RAM; let r = borrow x read; release r; look(r); }"),
        "borrow 'r' used after release",
    );
}

#[test]
fn writing_through_a_read_borrow_is_caught() {
    caught(
        "rt_write_read_borrow",
        &with("fn main() { buffer<u8, 8> x @RAM; let r = borrow x read; poke(r); release r; }"),
        "read borrow 'r' passed to 'poke', which writes it",
    );
}

#[test]
fn owner_written_while_borrowed_is_caught() {
    caught(
        "rt_owner_borrowed",
        &with("fn main() { buffer<u8, 8> x @RAM; let r = borrow x read; poke(x); release r; }"),
        "'x' passed to 'poke' while borrowed",
    );
}

#[test]
fn aliased_write_is_caught() {
    caught(
        "rt_alias",
        &with("fn main() { buffer<u8, 8> x @RAM; fir(x, x); }"),
        "the same buffer is passed to 'fir' twice",
    );
}

#[test]
fn broken_signature_promise_is_caught() {
    caught(
        "rt_promise",
        &with("fn wait(b: buffer<u8, 8> @DMA -> @RAM) { } fn main() { buffer<u8, 8> x @RAM; start(x); wait(x); sync(x); }"),
        "parameter 'b' leaves 'wait' in @DMA, but its signature says @RAM",
    );
}

#[test]
fn unreleased_borrow_is_caught() {
    caught(
        "rt_unreleased",
        &with("fn main() { buffer<u8, 8> x @RAM; let r = borrow x read; look(r); }"),
        "borrow 'r' goes out of scope without being released",
    );
}

#[test]
fn swap_while_borrowed_is_caught() {
    caught(
        "rt_swap_borrowed",
        &with("fn main() { buffer<u8, 8> a @RAM; buffer<u8, 8> b @RAM; let r = borrow a read; swap a, b; release r; }"),
        "'a' has a live borrow; cannot swap it",
    );
}

#[test]
fn borrowing_a_dma_buffer_is_caught() {
    caught(
        "rt_borrow_dma",
        &with("fn main() { buffer<u8, 8> x @RAM; start(x); let r = borrow x read; release r; sync(x); }"),
        "borrow 'r' of a buffer owned by @DMA",
    );
}

#[test]
fn a_correct_program_runs_clean_on_every_path() {
    let src = with(
        r#"
        extern fn flip() -> bool;
        fn main() {
            buffer<u8, 8> ping @RAM;
            buffer<u8, 8> pong @RAM;
            let n: u32 = 0;
            start(ping);
            while n < 3 {
                sync(ping);
                start(pong);
                look(ping);
                swap ping, pong;
                n = n + 1;
            }
            let c: bool = flip();
            if c {
                let r = borrow pong read;
                look(r);
                look(pong);
                release r;
            }
            sync(ping);
        }
        "#,
    );
    for seed in 1..=4 {
        let (code, stderr) = run_unanalyzed("rt_clean", &src, seed);
        assert_eq!(code, 0, "seed {seed}:\n{stderr}");
    }
}

#[test]
fn every_buildable_example_runs_clean_when_checked() {
    let root = workspace_root();
    for stem in [
        "radio_pipeline",
        "branching_pipeline",
        "counting_loop",
        "mmio_poll",
        "mmio_registers",
        "sdr_session",
        "overlapped_capture",
        "ping_pong",
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_phase"))
            .args(["build", "--checked"])
            .arg(root.join(format!("examples/{stem}.phase")))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{stem}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let run = Command::new(root.join(format!("build/{stem}-checked")))
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{stem}-checked failed:\n{}",
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

#[test]
fn a_callee_using_a_parameter_granted_by_a_callers_borrow_is_not_a_violation() {
    // The caller's write borrow is what grants `helper` access to `p`; it
    // must not count against `helper`'s own use of `p` (borrow checks are
    // per call frame). Was a false alarm in the first version of the
    // runtime checker, found by the fuzzer.
    let src = with(
        r#"
        fn helper(p: read buffer<u8, 8> @RAM) { look(p); let r = borrow p read; look(r); look(p); release r; }
        fn mover(p: buffer<u8, 8> @RAM) { move p -> @DMA; sync(p); poke(p); }
        fn main() {
            buffer<u8, 8> x @RAM;
            let w = borrow x write;
            helper(w);
            mover(w);
            release w;
        }
        "#,
    );
    let (code, stderr) = run_unanalyzed("rt_frames", &src, 1);
    assert_eq!(code, 0, "{stderr}");
}

#[test]
fn a_callee_still_conflicts_with_its_own_borrows() {
    caught(
        "rt_own_frame",
        &with(
            "fn helper(p: buffer<u8, 8> @RAM) { let r = borrow p read; poke(p); release r; } fn main() { buffer<u8, 8> x @RAM; helper(x); }",
        ),
        "'p' passed to 'poke' while borrowed",
    );
}
