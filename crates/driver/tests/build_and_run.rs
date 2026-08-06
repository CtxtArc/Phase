//! M5 end-to-end test: proves the *entire* pipeline works, not just each
//! stage in isolation -- parse -> analyze -> PIR -> C codegen -> `cc` ->
//! actually run the resulting binary -> check its real output. This is
//! the strongest possible evidence the milestone's stated deliverable
//! ("a binary that actually runs the simulated capture -> filter ->
//! playback sequence") is real and not just plausible-looking code.

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

#[test]
fn radio_pipeline_builds_and_runs_end_to_end() {
    let root = workspace_root();
    let src_path = root.join("examples/radio_pipeline.phase");
    let src = std::fs::read_to_string(&src_path).expect("radio_pipeline.phase must exist");

    // parse -> analyze -> PIR -> C, exactly like `phase build` does.
    let program = phase_parser::parse(&src).expect("must parse");
    phase_analysis::analyze(&program).expect("must pass analysis");
    let pir = phase_pir::build(&program).expect("must lower to PIR (M5 scope: straight-line only)");
    // Must match the stem runtime/radio_pipeline_extern.c hardcodes in its
    // `#include "radio_pipeline.gen.h"` -- that hardcoded include is a
    // deliberate part of the demo runtime's design (spec §5.3: it's
    // program-specific glue, not a generic runtime), not a detail this
    // test can vary independently.
    let generated = phase_codegen_c::generate(&pir, "radio_pipeline");

    let build_dir = root.join("build");
    std::fs::create_dir_all(&build_dir).unwrap();
    let header_path = build_dir.join("radio_pipeline.gen.h");
    let source_path = build_dir.join("radio_pipeline.gen.c");
    std::fs::write(&header_path, &generated.header).unwrap();
    std::fs::write(&source_path, &generated.source).unwrap();

    let generic_runtime_c = root.join("runtime/phase_runtime.c");
    let extern_runtime_c = root.join("runtime/radio_pipeline_extern.c");
    let runtime_dir = root.join("runtime");
    let binary_path = build_dir.join("radio_pipeline_test_bin");

    let cc_output = Command::new("cc")
        .args([
            "-std=c11",
            "-Wall",
            "-Werror",
            "-I",
        ])
        .arg(&build_dir)
        .args(["-I"])
        .arg(&runtime_dir)
        .args(["-o"])
        .arg(&binary_path)
        .arg(&source_path)
        .arg(&generic_runtime_c)
        .arg(&extern_runtime_c)
        .arg("-lm")
        .output()
        .expect("failed to invoke cc -- is a C compiler installed?");

    assert!(
        cc_output.status.success(),
        "cc failed to compile the generated C:\n{}",
        String::from_utf8_lossy(&cc_output.stderr)
    );
    assert!(
        cc_output.stderr.is_empty(),
        "cc produced warnings (build with -Werror should have zero):\n{}",
        String::from_utf8_lossy(&cc_output.stderr)
    );

    let run_output = Command::new(&binary_path)
        .output()
        .expect("failed to run the compiled binary");

    assert!(
        run_output.status.success(),
        "the compiled binary exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&run_output.stdout);

    // The real pipeline actually ran, in the right order: capture, wait
    // for the DMA transfer, filter, play back computed statistics, wait
    // for the device transfer.
    let capture_pos = stdout.find("device_capture: filling 1024 samples").expect("capture trace missing");
    let dma_wait_pos = stdout.find("sim_dma_wait(\"raw\"): DMA transfer complete").expect("DMA wait trace missing");
    let filter_pos = stdout.find("fir_filter: filtering 1024 -> 1024 samples").expect("filter trace missing");
    let playback_pos = stdout.find("device_playback: streaming 1024 samples").expect("playback trace missing");
    let device_wait_pos = stdout
        .find("sim_device_wait(\"filtered\"): device transfer complete")
        .expect("device wait trace missing");

    assert!(capture_pos < dma_wait_pos, "capture must happen before the DMA sync completes");
    assert!(dma_wait_pos < filter_pos, "DMA sync must complete before filtering");
    assert!(filter_pos < playback_pos, "filtering must happen before playback");
    assert!(playback_pos < device_wait_pos, "playback must happen before the device sync completes");

    // The filter actually ran on real data, not stub zeros: playback's
    // reported RMS should be a real, non-trivial nonzero number.
    let playback_line = stdout.lines().find(|l| l.contains("device_playback")).unwrap();
    assert!(playback_line.contains("rms="), "playback line missing rms stat: {playback_line}");
    let rms_str = playback_line.split("rms=").nth(1).unwrap().trim_end_matches(')').trim();
    let rms: f64 = rms_str.parse().expect("rms should be a parseable number");
    assert!(rms > 0.01, "expected a real nonzero RMS from actual filtered signal data, got {rms}");
}

/// M8 end-to-end test: `examples/branching_pipeline.phase` builds with
/// real C `if`/`else` (closing M5's straight-line-only scope cut) and both
/// branches actually run, selected by an environment variable at runtime
/// -- not just that the M4 analyzer accepts the branching source.
///
/// `variant` picks a distinct build subdirectory per caller (e.g. "dma",
/// "device") -- `cargo test` runs tests in parallel by default, and the
/// generated source's `#include "branching_pipeline.gen.h"` is a fixed
/// name (it has to match the stem passed to `codegen_c::generate`), so
/// two tests sharing one `build/` directory would race on the same
/// `.gen.h`/`.gen.c`/binary files and could non-deterministically compile
/// or run the wrong content.
fn build_branching_pipeline(variant: &str, use_dma_env: Option<&str>) -> std::process::Output {
    let root = workspace_root();
    let src_path = root.join("examples/branching_pipeline.phase");
    let src = std::fs::read_to_string(&src_path).expect("branching_pipeline.phase must exist");

    let program = phase_parser::parse(&src).expect("must parse");
    phase_analysis::analyze(&program).expect("must pass analysis");
    let pir = phase_pir::build(&program).expect("M8: control flow must now lower to PIR");
    let generated = phase_codegen_c::generate(&pir, "branching_pipeline");

    let build_dir = root.join("build").join(format!("branching_pipeline_{variant}"));
    std::fs::create_dir_all(&build_dir).unwrap();
    let header_path = build_dir.join("branching_pipeline.gen.h");
    let source_path = build_dir.join("branching_pipeline.gen.c");
    std::fs::write(&header_path, &generated.header).unwrap();
    std::fs::write(&source_path, &generated.source).unwrap();

    let generic_runtime_c = root.join("runtime/phase_runtime.c");
    let extern_runtime_c = root.join("runtime/branching_pipeline_extern.c");
    let runtime_dir = root.join("runtime");
    let binary_path = build_dir.join("branching_pipeline_test_bin");

    let cc_output = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-I"])
        .arg(&build_dir)
        .args(["-I"])
        .arg(&runtime_dir)
        .args(["-o"])
        .arg(&binary_path)
        .arg(&source_path)
        .arg(&generic_runtime_c)
        .arg(&extern_runtime_c)
        .output()
        .expect("failed to invoke cc -- is a C compiler installed?");

    assert!(
        cc_output.status.success() && cc_output.stderr.is_empty(),
        "cc failed (or warned) compiling the generated branching C:\n{}",
        String::from_utf8_lossy(&cc_output.stderr)
    );

    let mut cmd = Command::new(&binary_path);
    if let Some(v) = use_dma_env {
        cmd.env("PHASE_DEMO_USE_DMA", v);
    }
    cmd.output().expect("failed to run the compiled binary")
}

#[test]
fn branching_pipeline_dma_path_runs_the_if_branch() {
    let out = build_branching_pipeline("dma", Some("1"));
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("if-branch taken"), "stdout: {stdout}");
    assert!(!stdout.contains("else-branch taken"), "stdout: {stdout}");
    assert!(stdout.contains("sim_dma_wait"), "stdout: {stdout}");
}

#[test]
fn branching_pipeline_device_path_runs_the_else_branch() {
    let out = build_branching_pipeline("device", Some("0"));
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("else-branch taken"), "stdout: {stdout}");
    assert!(!stdout.contains("if-branch taken"), "stdout: {stdout}");
    assert!(stdout.contains("sim_device_wait"), "stdout: {stdout}");
}

/// M9 end-to-end test: `examples/counting_loop.phase` builds and actually
/// runs a `while` loop that terminates on its own via a real `n = n + 1;`
/// assignment (spec §9, Milestone M9) -- not a loop that's merely
/// accepted by analysis, an actually-running, actually-bounded one.
#[test]
fn counting_loop_builds_and_runs_exactly_five_iterations() {
    let root = workspace_root();
    let src_path = root.join("examples/counting_loop.phase");
    let src = std::fs::read_to_string(&src_path).expect("counting_loop.phase must exist");

    let program = phase_parser::parse(&src).expect("must parse");
    phase_analysis::analyze(&program).expect("must pass analysis");
    let pir = phase_pir::build(&program).expect("M9: assignment must now lower to PIR");
    let generated = phase_codegen_c::generate(&pir, "counting_loop");

    // A distinct subdirectory name (not just the stem) -- `phase build`
    // itself writes its output binary to `build/counting_loop`, and this
    // test must not collide with that if someone's run the CLI by hand.
    let build_dir = root.join("build").join("counting_loop_driver_test");
    std::fs::create_dir_all(&build_dir).unwrap();
    let header_path = build_dir.join("counting_loop.gen.h");
    let source_path = build_dir.join("counting_loop.gen.c");
    std::fs::write(&header_path, &generated.header).unwrap();
    std::fs::write(&source_path, &generated.source).unwrap();

    let generic_runtime_c = root.join("runtime/phase_runtime.c");
    let extern_runtime_c = root.join("runtime/counting_loop_extern.c");
    let runtime_dir = root.join("runtime");
    let binary_path = build_dir.join("counting_loop_test_bin");

    let cc_output = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-I"])
        .arg(&build_dir)
        .args(["-I"])
        .arg(&runtime_dir)
        .args(["-o"])
        .arg(&binary_path)
        .arg(&source_path)
        .arg(&generic_runtime_c)
        .arg(&extern_runtime_c)
        .output()
        .expect("failed to invoke cc -- is a C compiler installed?");

    assert!(
        cc_output.status.success() && cc_output.stderr.is_empty(),
        "cc failed (or warned) compiling the generated counting-loop C:\n{}",
        String::from_utf8_lossy(&cc_output.stderr)
    );

    let out = Command::new(&binary_path)
        .output()
        .expect("failed to run the compiled binary");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);

    // Exactly n=0..4 -- proves the loop terminates on its own rather than
    // running zero times (condition never true) or being cut off by
    // something external.
    for n in 0..5 {
        assert!(stdout.contains(&format!("n={n}")), "stdout: {stdout}");
    }
    assert!(!stdout.contains("n=5"), "stdout: {stdout}");
}
