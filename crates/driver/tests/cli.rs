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
    assert!(String::from_utf8_lossy(&help.stdout).contains("check <file>"));

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

#[test]
fn human_errors_show_the_source_line_and_notes() {
    let root = workspace_root();
    let out = phase()
        .arg("--color=never")
        .arg("check")
        .arg(root.join("examples/bug_gallery/stale_typestate_reuse.phase"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("stale_typestate_reuse.phase:22:5"), "{err}");
    assert!(err.contains("22 |     let again = decode(p1);"), "{err}");
    assert!(err.contains("   |                        ^^\n"), "{err}");
    assert!(err.contains("note: 'p1' is consumed here"), "{err}");
    assert!(
        !err.contains('\x1b'),
        "--color=never must not emit ANSI codes"
    );
}

#[test]
fn short_errors_are_one_line_each() {
    let root = workspace_root();
    let out = phase()
        .args(["--error-format=short", "check"])
        .arg(root.join("examples/bug_gallery/destroy_while_dma.phase"))
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    let first = err.lines().next().unwrap();
    assert!(
        first.ends_with(
            "destroy_while_dma.phase:14:5: error: entity 'rx' is still owned by @DMA; sync(rx) before destroying it"
        ),
        "{err}"
    );
    assert!(err.contains("destroy_while_dma.phase:10:5: note: 'rx' is moved to @DMA here"));
}

#[test]
fn color_always_emits_ansi_and_bad_flags_are_rejected() {
    let root = workspace_root();
    let colored = phase()
        .args(["--color=always", "check"])
        .arg(root.join("examples/bug_gallery/use_after_release.phase"))
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&colored.stderr).contains("\x1b[1;31merror"));

    let bad = phase()
        .args(["--error-format=json", "check", "x.phase"])
        .output()
        .unwrap();
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("unknown --error-format"));
}

#[test]
fn parse_errors_are_rendered_with_their_location() {
    let dir = std::env::temp_dir().join("phase_cli_parse_error");
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("bad.phase");
    std::fs::write(&file, "fn main() {\n    let x: u32 = 1\n}\n").unwrap();
    let out = phase()
        .args(["--color=never", "check"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("bad.phase:3:1"), "{err}");
    assert!(err.contains("3 | }"), "{err}");
}
