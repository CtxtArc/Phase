//! `phase-fuzz`: differential fuzzing of PHASE's static analysis against
//! runtime-checked execution. See the crate docs (src/lib.rs).
//!
//!   cargo run --release -p phase_fuzz -- [--count N] [--seed S] [--runs R]
//!                                        [--chaos P] [--out DIR]
//!
//! Exit status is 1 if any accepted program tripped a runtime check (or
//! failed later in the pipeline); each such program is written to DIR.

use phase_fuzz::{fuzz, gen::Config, Runner};
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut count = 200usize;
    let mut seed = 1u64;
    let mut runs = 4u32;
    let mut cfg = Config::default();
    let mut out = PathBuf::from("fuzz-findings");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let val = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--count" => count = val.parse().unwrap_or(count),
            "--seed" => seed = val.parse().unwrap_or(seed),
            "--runs" => runs = val.parse().unwrap_or(runs),
            "--chaos" => cfg.chaos = val.parse().unwrap_or(cfg.chaos),
            "--out" => out = PathBuf::from(val),
            "--help" | "-h" => {
                println!(
                    "usage: phase-fuzz [--count N] [--seed S] [--runs R] [--chaos P] [--out DIR]"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument '{other}'");
                return ExitCode::FAILURE;
            }
        }
        i += 2;
    }

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let runner = Runner::new(&root, std::env::temp_dir().join("phase-fuzz-work"), runs);
    let s = fuzz(&runner, seed, count, cfg);

    println!(
        "{} programs generated, {} accepted by `phase check` ({} more only with warnings, not run), {} soundness violations, {} pipeline failures",
        s.generated,
        s.accepted,
        s.warned,
        s.violations.len(),
        s.failures.len()
    );
    if s.violations.is_empty() && s.failures.is_empty() {
        return ExitCode::SUCCESS;
    }
    let _ = std::fs::create_dir_all(&out);
    for (kind, list) in [("violation", &s.violations), ("failure", &s.failures)] {
        for (seed, msg, src) in list {
            let file = out.join(format!("{kind}-{seed}.phase"));
            let commented: String = msg.lines().map(|l| format!("// {l}\n")).collect();
            let _ = std::fs::write(&file, format!("{commented}\n{src}"));
            println!("{kind}: seed {seed}: {}", msg.lines().next().unwrap_or(""));
            println!("    saved to {}", file.display());
        }
    }
    ExitCode::FAILURE
}
