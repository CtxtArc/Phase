//! Differential fuzzing for PHASE's static analysis.
//!
//! [`gen::program`] builds random programs. For each one `phase check`
//! (the static analyzer) decides; every program it *accepts* is compiled as
//! a runtime-checked build (`phase build --checked`, with stub externs) and
//! run with several stub seeds, so `flip()`/`count()` drive it down
//! different paths. The runtime checker (runtime/phase_check.c) is
//! independent of the analyzer's reasoning, so an accepted program that
//! trips it is a soundness bug in the analyzer -- a [`Outcome::Violation`].

pub mod gen;

use std::path::{Path, PathBuf};
use std::process::Command;

/// What happened to one generated program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The analyzer rejected it (the common, uninteresting case).
    Rejected,
    /// Accepted only with warnings: PHASE does not vouch for these (a
    /// warning marks something it cannot check), so they are not run.
    Warned,
    /// Accepted, and every run passed the runtime checks.
    Clean,
    /// Accepted, but a run tripped a runtime check: an analyzer soundness bug.
    Violation { stub_seed: u32, message: String },
    /// Accepted, but lowering, C compilation, or a run failed some other
    /// way: a bug in the pipeline after the analyzer.
    PipelineFailure(String),
}

/// Where to find the runtime and where to put scratch builds.
pub struct Runner {
    pub runtime_dir: PathBuf,
    pub work_dir: PathBuf,
    /// Stub seeds each accepted program is run with.
    pub runs: u32,
}

impl Runner {
    pub fn new(workspace_root: &Path, work_dir: PathBuf, runs: u32) -> Self {
        Runner {
            runtime_dir: workspace_root.join("runtime"),
            work_dir,
            runs,
        }
    }

    /// Check, build, and run one program.
    pub fn run(&self, name: &str, src: &str) -> Outcome {
        let program = match phase_parser::parse(src) {
            Ok(p) => p,
            Err(e) => {
                return Outcome::PipelineFailure(format!("generated an unparsable program: {e}"))
            }
        };
        let report = phase_analysis::analyze_full(&program);
        if !report.errors.is_empty() {
            return Outcome::Rejected;
        }
        if !report.warnings.is_empty() {
            return Outcome::Warned;
        }
        let pir = match phase_pir::build(&program) {
            Ok(p) => p,
            Err(e) => return Outcome::PipelineFailure(format!("lowering failed: {e}")),
        };

        let dir = self.work_dir.join(name);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return Outcome::PipelineFailure(format!("cannot create {}: {e}", dir.display()));
        }
        let g = phase_codegen_c::generate_checked(&pir, name);
        let files = [
            (format!("{name}.gen.h"), g.header),
            (format!("{name}.checked.gen.c"), g.source),
            (
                format!("{name}.stubs.c"),
                phase_codegen_c::generate_stubs(&pir, name),
            ),
        ];
        for (file, text) in &files {
            if let Err(e) = std::fs::write(dir.join(file), text) {
                return Outcome::PipelineFailure(format!("cannot write {file}: {e}"));
            }
        }

        let binary = dir.join(name);
        let cc = Command::new("cc")
            .args(["-std=c11", "-O0", "-w", "-I"])
            .arg(&dir)
            .arg("-I")
            .arg(&self.runtime_dir)
            .arg("-o")
            .arg(&binary)
            .arg(dir.join(&files[1].0))
            .arg(dir.join(&files[2].0))
            .arg(self.runtime_dir.join("phase_runtime.c"))
            .arg(self.runtime_dir.join("phase_check.c"))
            .arg("-lm")
            .output();
        match cc {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                return Outcome::PipelineFailure(format!(
                    "cc rejected the generated C:\n{}",
                    String::from_utf8_lossy(&out.stderr)
                ))
            }
            Err(e) => return Outcome::PipelineFailure(format!("could not run cc: {e}")),
        }

        for stub_seed in 1..=self.runs {
            let out = match Command::new(&binary)
                .env("PHASE_STUB_SEED", stub_seed.to_string())
                .output()
            {
                Ok(o) => o,
                Err(e) => return Outcome::PipelineFailure(format!("could not run binary: {e}")),
            };
            match out.status.code() {
                Some(0) => {}
                Some(70) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    let message = stderr
                        .lines()
                        .find(|l| l.starts_with("PHASE runtime check failed"))
                        .unwrap_or("PHASE runtime check failed")
                        .to_string();
                    return Outcome::Violation { stub_seed, message };
                }
                other => {
                    return Outcome::PipelineFailure(format!(
                        "binary exited with {other:?} (stub seed {stub_seed}):\n{}",
                        String::from_utf8_lossy(&out.stderr)
                    ))
                }
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
        Outcome::Clean
    }
}

/// Totals for a fuzzing session.
#[derive(Debug, Default, Clone)]
pub struct Summary {
    pub generated: usize,
    pub accepted: usize,
    pub warned: usize,
    pub violations: Vec<(u64, String, String)>,
    pub failures: Vec<(u64, String, String)>,
}

/// Generate and test `count` programs from seeds `first_seed..`.
pub fn fuzz(runner: &Runner, first_seed: u64, count: usize, cfg: gen::Config) -> Summary {
    let mut s = Summary::default();
    for seed in first_seed..first_seed + count as u64 {
        let src = gen::program(seed, cfg);
        s.generated += 1;
        match runner.run(&format!("fuzz{seed}"), &src) {
            Outcome::Rejected => {}
            Outcome::Warned => s.warned += 1,
            Outcome::Clean => s.accepted += 1,
            Outcome::Violation { stub_seed, message } => {
                s.accepted += 1;
                s.violations
                    .push((seed, format!("{message} (stub seed {stub_seed})"), src));
            }
            Outcome::PipelineFailure(msg) => {
                s.accepted += 1;
                s.failures.push((seed, msg, src));
            }
        }
    }
    s
}
