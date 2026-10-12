//! End-to-end tests of the `phase` binary itself (argument handling, exit
//! codes, and `build` path resolution), as opposed to the library pipeline
//! the other integration tests drive directly.

use std::path::PathBuf;
use std::process::Command;

fn phase() -> Command {
    Command::new(env!("CARGO_BIN_EXE_phase"))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crates/driver must sit two levels below the workspace root")
        .to_path_buf()
}

#[test]
fn version_matches_the_crate_version() {
    let out = phase().arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("phase {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_succeeds_and_no_args_fails() {
    let help = phase().arg("--help").output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("phase check <file>"));

    let none = phase().output().unwrap();
    assert!(!none.status.success());
    assert!(String::from_utf8_lossy(&none.stderr).contains("usage:"));
}

#[test]
fn check_exit_code_reflects_the_verdict() {
    let root = workspace_root();
    let ok = phase()
        .arg("check")
        .arg(root.join("examples/sdr_session.phase"))
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "{}",
        String::from_utf8_lossy(&ok.stderr)
    );

    let bad = phase()
        .arg("check")
        .arg(root.join("examples/bug_gallery/forged_typestate.phase"))
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&bad.stderr).contains(
        "'p' is declared as Packet<Validated> but 'receive_packet' returns Packet<Received>"
    ));
}

#[test]
fn missing_file_is_a_clean_error() {
    let out = phase()
        .args(["check", "/definitely/not/here.phase"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("could not read"));
}

#[test]
fn build_works_from_an_unrelated_working_directory() {
    let root = workspace_root();
    let elsewhere = std::env::temp_dir();
    let out = phase()
        .current_dir(&elsewhere)
        .env_remove("PHASE_RUNTIME_DIR")
        .arg("build")
        .arg(root.join("examples/counting_loop.phase"))
        .output()
        .expect("cc must be available for this test");
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let binary = root.join("build/counting_loop");
    let run = Command::new(&binary).output().unwrap();
    assert!(run.status.success());
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(
        stdout.matches("tick").count(),
        5,
        "expected 5 ticks, got:\n{stdout}"
    );
}
