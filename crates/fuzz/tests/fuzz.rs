//! Differential fuzzing in the test suite: replay every program the fuzzer
//! has ever flagged, then fuzz a fresh batch. A violation here means the
//! static analyzer accepted a program that breaks the ownership rules at
//! run time.

use phase_fuzz::{fuzz, gen, Outcome, Runner};
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

fn runner(tag: &str) -> Runner {
    Runner::new(&root(), root().join("build").join(tag), 4)
}

#[test]
fn past_findings_stay_fixed() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("regressions");
    let r = runner("fuzz_regressions");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        let src = std::fs::read_to_string(&path).unwrap();
        let outcome = r.run(&name, &src);
        assert!(
            matches!(outcome, Outcome::Rejected | Outcome::Clean),
            "{}: {outcome:?}",
            path.display()
        );
        // Each file's header says which way it must go.
        let expect_rejected = src.contains("Must now be rejected");
        assert_eq!(
            outcome == Outcome::Rejected,
            expect_rejected,
            "{}: {outcome:?}",
            path.display()
        );
        n += 1;
    }
    assert!(n >= 5);
}

#[test]
fn a_fresh_batch_of_random_programs_has_no_soundness_violations() {
    let s = fuzz(&runner("fuzz_smoke"), 1, 120, gen::Config::default());
    assert!(
        s.violations.is_empty() && s.failures.is_empty(),
        "violations: {:#?}\nfailures: {:#?}",
        s.violations,
        s.failures
    );
    // The generator must keep producing a useful share of accepted
    // programs, or the test proves nothing.
    assert!(
        s.accepted * 4 >= s.generated,
        "{} / {} accepted",
        s.accepted,
        s.generated
    );
}

#[test]
fn generation_is_deterministic() {
    let cfg = gen::Config::default();
    assert_eq!(gen::program(42, cfg), gen::program(42, cfg));
    assert_ne!(gen::program(42, cfg), gen::program(43, cfg));
}
