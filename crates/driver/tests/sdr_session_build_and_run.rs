//! End-to-end test for examples/sdr_session.phase, the M7 worked example
//! combining domain safety, typestate, and volatile MMIO in one program.
//! Mirrors build_and_run.rs's radio_pipeline test: this doesn't just parse
//! and analyze the source, it compiles the generated C with a real C
//! compiler, links it against runtime/sdr_session_extern.c, runs the
//! resulting binary, and checks its real output -- proving the "features
//! compose" claim is backed by an executable, not just static analysis.

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
fn sdr_session_builds_and_runs_end_to_end() {
    let root = workspace_root();
    let src_path = root.join("examples/sdr_session.phase");
    let src = std::fs::read_to_string(&src_path).expect("sdr_session.phase must exist");

    let program = phase_parser::parse(&src).expect("must parse");
    phase_analysis::analyze(&program).expect("must pass analysis");
    let pir = phase_pir::build(&program).expect("must lower to PIR");
    let generated = phase_codegen_c::generate(&pir, "sdr_session");

    let build_dir = root.join("build");
    std::fs::create_dir_all(&build_dir).unwrap();
    std::fs::write(build_dir.join("sdr_session.gen.h"), &generated.header).unwrap();
    std::fs::write(build_dir.join("sdr_session.gen.c"), &generated.source).unwrap();

    let generic_runtime_c = root.join("runtime/phase_runtime.c");
    let extern_runtime_c = root.join("runtime/sdr_session_extern.c");
    let binary_path = build_dir.join("sdr_session_test_bin");

    let cc_output = Command::new("cc")
        .args(["-std=c11", "-Wall", "-Werror", "-I"])
        .arg(&build_dir)
        .args(["-I"])
        .arg(root.join("runtime"))
        .args(["-o"])
        .arg(&binary_path)
        .arg(build_dir.join("sdr_session.gen.c"))
        .arg(&generic_runtime_c)
        .arg(&extern_runtime_c)
        .arg("-lm")
        .output()
        .expect("failed to invoke cc");

    assert!(
        cc_output.status.success(),
        "cc failed to compile+link sdr_session:\n{}",
        String::from_utf8_lossy(&cc_output.stderr)
    );
    assert!(
        cc_output.stderr.is_empty(),
        "cc produced warnings (expected zero):\n{}",
        String::from_utf8_lossy(&cc_output.stderr)
    );

    let run_output = Command::new(&binary_path)
        .output()
        .expect("failed to run the compiled binary");
    assert!(
        run_output.status.success(),
        "binary exited non-zero:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );

    let stdout = String::from_utf8_lossy(&run_output.stdout);

    // All three mechanisms actually ran, in the right order: capture/
    // filter/playback, then the full typestate chain.
    let capture_pos = stdout.find("device_capture").expect("capture missing");
    let playback_pos = stdout.find("device_playback").expect("playback missing");
    let receive_pos = stdout.find("receive_packet").expect("receive_packet missing");
    let decode_pos = stdout.find("decode: sequence").expect("decode missing");
    let validate_pos = stdout.find("validate: sequence").expect("validate missing");
    let handle_pos = stdout.find("handle: sequence").expect("handle missing");

    assert!(capture_pos < playback_pos);
    assert!(receive_pos < decode_pos);
    assert!(decode_pos < validate_pos);
    assert!(validate_pos < handle_pos);
    assert!(stdout.contains("accepted"), "expected the packet to reach the accepted/Validated stage");
}
