// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton

//! `xtask` — one entry point for every check this repository can run locally.
//!
//! Stage 0 of `architecture/roadmap/BOTTOM-UP-IMPLEMENTATION-PLAN.md` asks for
//! commands that orchestrate the cargo-side checks and **call** the existing
//! `tools/checks` scripts rather than reimplementing them. Reimplementing them
//! would produce a second answer to every question the scripts already answer,
//! and the two would disagree exactly when it mattered.
//!
//! `xtask` is the developer-facing entry point. **CI keeps invoking the scripts
//! by name**, and that is not redundancy: `tools/checks/check_guards_are_wired.sh`
//! proves a guard is reachable by looking for its basename in a workflow, so
//! routing CI through `cargo run -p xtask` would hide every guard from the
//! check written to find unreachable guards.
//!
//! Usage:
//!
//! ```text
//! cargo xtask checks       # the tree checks under tools/checks/
//! cargo xtask selftests    # every test_*.sh beside its script
//! cargo xtask fmt [--check]
//! cargo xtask clippy
//! cargo xtask test
//! cargo xtask ci           # all of the above, --check for fmt
//! ```
//!
//! Exit code is 0 when every task passed, 1 when any failed, 2 for a usage
//! problem. A failing task does not stop the run: the point of a pre-push
//! command is to learn everything that is wrong in one pass.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// One thing to run, with the name it is reported under.
struct Task {
    label: String,
    program: String,
    args: Vec<String>,
}

impl Task {
    fn new(label: &str, program: &str, args: &[&str]) -> Self {
        Self {
            label: label.to_owned(),
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
        }
    }
}

/// The tree checks, in the order CI runs them.
///
/// Kept in step with `.github/workflows/ci.yml` by the `every_tree_check_is_run`
/// test below, which reads `tools/checks/` from disk rather than trusting this
/// list: a check added to the directory and forgotten here would be a guard
/// that a developer's pre-push run silently skips.
fn tree_checks() -> Vec<Task> {
    vec![
        Task::new(
            "ADR index, template, and amendment record",
            "bash",
            &["tools/checks/validate_adr_index.sh"],
        ),
        Task::new(
            "semantic collisions between parallel branches",
            "bash",
            &["tools/checks/scan_semantic_collisions.sh"],
        ),
        Task::new(
            "licence headers and foreign licence terms",
            "bash",
            &["tools/checks/check_license_headers.sh"],
        ),
        Task::new(
            "stage status agrees across the tree",
            "bash",
            &["tools/checks/check_stage_status.sh"],
        ),
        Task::new(
            "component READMEs agree with their own directories",
            "bash",
            &["tools/checks/check_component_status.sh"],
        ),
        Task::new(
            "required check contexts match the workflow's job names",
            "bash",
            &["tools/checks/check_required_contexts.sh"],
        ),
        Task::new(
            "dependency policy — advisories, licences, bans, sources",
            "bash",
            &["tools/checks/check_dependencies.sh"],
        ),
        Task::new(
            "wire contracts — schema, manifest, provenance",
            "python3",
            &["tools/checks/validate_contracts.py"],
        ),
        Task::new(
            "frozen fixture vectors recompute",
            "python3",
            &["tools/checks/verify_fixture_vectors.py"],
        ),
        Task::new(
            "Markdown links resolve and YAML parses",
            "python3",
            &["tools/checks/check_docs_integrity.py"],
        ),
        Task::new(
            "every guard is reachable from a workflow",
            "bash",
            &["tools/checks/check_guards_are_wired.sh"],
        ),
    ]
}

/// Every `test_*.sh` beside the script it tests, discovered rather than listed.
fn self_tests(root: &Path) -> Result<Vec<Task>, String> {
    let mut found: Vec<String> = Vec::new();
    for dir in ["tools/checks", "tools/gh"] {
        let path = root.join(dir);
        let entries =
            fs::read_dir(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with("test_") && name.ends_with(".sh") {
                found.push(format!("{dir}/{name}"));
            }
        }
    }
    found.sort();
    Ok(found
        .into_iter()
        .map(|rel| Task::new("self-test", "bash", &[rel.as_str()]))
        .collect())
}

/// The cargo that invoked us, when there is one.
///
/// `cargo xtask` sets `CARGO`, and honouring it is what keeps the nested
/// `cargo clippy` on the same pinned toolchain instead of whichever cargo the
/// PATH happens to resolve.
fn cargo() -> String {
    env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

fn cargo_task(label: &str, args: &[&str]) -> Task {
    Task::new(label, &cargo(), args)
}

fn fmt_task(check: bool) -> Task {
    if check {
        cargo_task("cargo fmt --check", &["fmt", "--all", "--check"])
    } else {
        cargo_task("cargo fmt", &["fmt", "--all"])
    }
}

fn clippy_task() -> Task {
    cargo_task(
        "cargo clippy",
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
    )
}

fn test_task() -> Task {
    cargo_task("cargo test", &["test", "--workspace", "--all-targets"])
}

/// Run every task, reporting each, and return the number that failed.
///
/// Deliberately does not short-circuit. A pre-push command that stops at the
/// first failure turns one run into as many runs as there are problems.
fn run_all(root: &Path, tasks: Vec<Task>) -> usize {
    let mut failures: Vec<String> = Vec::new();

    for task in tasks {
        let mut shown = String::new();
        let _ = write!(shown, "{}", task.program);
        for arg in &task.args {
            let _ = write!(shown, " {arg}");
        }
        println!("\n== {} :: {shown}", task.label);

        let status = Command::new(&task.program)
            .args(&task.args)
            .current_dir(root)
            .status();

        match status {
            Ok(status) if status.success() => {}
            Ok(status) => failures.push(format!("{shown} exited {}", describe(status))),
            // A missing interpreter is a real failure and not an excuse to
            // pass: `python3` absent means the contract and fixture checks did
            // not run, which is indistinguishable from them not existing.
            Err(e) => failures.push(format!("{shown} could not be started: {e}")),
        }
    }

    if failures.is_empty() {
        println!("\nxtask: OK — every task passed.");
    } else {
        eprintln!("\nxtask: {} task(s) failed:", failures.len());
        for f in &failures {
            eprintln!("  - {f}");
        }
    }
    failures.len()
}

fn describe(status: std::process::ExitStatus) -> String {
    match status.code() {
        Some(code) => code.to_string(),
        None => "on a signal".to_owned(),
    }
}

/// The repository root, derived from this package rather than the cwd, so
/// `cargo xtask` behaves the same from any subdirectory.
fn repo_root() -> Result<PathBuf, String> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no parent directory", manifest.display()))
}

fn usage() -> &'static str {
    "usage: cargo xtask <checks|selftests|fmt [--check]|clippy|test|ci>"
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let root = match repo_root() {
        Ok(root) => root,
        Err(e) => {
            eprintln!("xtask: {e}");
            return ExitCode::from(2);
        }
    };

    let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
    let tasks = match args.first().map(String::as_str) {
        Some("checks") => tree_checks(),
        Some("selftests") => match self_tests(&root) {
            Ok(tasks) => tasks,
            Err(e) => {
                eprintln!("xtask: {e}");
                return ExitCode::from(2);
            }
        },
        Some("fmt") => vec![fmt_task(rest.contains(&"--check"))],
        Some("clippy") => vec![clippy_task()],
        Some("test") => vec![test_task()],
        Some("ci") => {
            let mut tasks = vec![fmt_task(true), clippy_task(), test_task()];
            tasks.extend(tree_checks());
            match self_tests(&root) {
                Ok(selftests) => tasks.extend(selftests),
                Err(e) => {
                    eprintln!("xtask: {e}");
                    return ExitCode::from(2);
                }
            }
            tasks
        }
        Some("-h" | "--help" | "help") => {
            println!("{}", usage());
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("xtask: unknown command: {other}\n{}", usage());
            return ExitCode::from(2);
        }
        None => {
            eprintln!("xtask: no command given\n{}", usage());
            return ExitCode::from(2);
        }
    };

    if run_all(&root, tasks) == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    /// Every guard under `tools/checks/` is in `tree_checks()`.
    ///
    /// The list is written out for ordering and labels, which means it can go
    /// stale the moment a check is added — and a stale list fails the way that
    /// is hardest to notice, by running fewer checks and still printing OK.
    /// Discovered from disk here so adding a guard without wiring it fails a
    /// test instead of quietly narrowing the local run.
    #[test]
    fn every_tree_check_is_run() {
        let root = repo_root().expect("the xtask package has a parent directory");
        let wired: Vec<String> = tree_checks()
            .iter()
            .flat_map(|t| t.args.clone())
            .filter(|a| a.starts_with("tools/checks/"))
            .collect();

        let mut missing: Vec<String> = Vec::new();
        for entry in fs::read_dir(root.join("tools/checks")).expect("tools/checks is readable") {
            let entry = entry.expect("tools/checks entry is readable");
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            // Self-tests belong to `selftests`; data files are not guards.
            if name.starts_with("test_") {
                continue;
            }
            if !matches!(
                Path::new(name).extension().and_then(OsStr::to_str),
                Some("sh" | "py")
            ) {
                continue;
            }
            let rel = format!("tools/checks/{name}");
            if !wired.contains(&rel) {
                missing.push(rel);
            }
        }
        missing.sort();
        assert!(
            missing.is_empty(),
            "these guards exist but `cargo xtask checks` does not run them: {missing:?}"
        );
    }

    /// `selftests` finds the suites, and finds them in both directories.
    #[test]
    fn self_tests_are_discovered() {
        let root = repo_root().expect("the xtask package has a parent directory");
        let found = self_tests(&root).expect("tools/checks and tools/gh are readable");
        let paths: Vec<&String> = found.iter().flat_map(|t| t.args.iter()).collect();

        assert!(
            paths.iter().any(|p| p.starts_with("tools/checks/")),
            "no tools/checks self-test discovered: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p.starts_with("tools/gh/")),
            "no tools/gh self-test discovered: {paths:?}"
        );
        assert!(
            paths.iter().all(|p| p.contains("/test_")),
            "a non-self-test was picked up: {paths:?}"
        );
    }

    /// `fmt` writes by default and only checks when asked. Getting this
    /// backwards would make `cargo xtask ci` reformat the tree it is verifying.
    #[test]
    fn fmt_check_is_opt_in() {
        assert!(fmt_task(true).args.contains(&"--check".to_owned()));
        assert!(!fmt_task(false).args.contains(&"--check".to_owned()));
    }

    /// Clippy failures must fail the command; a warning nobody fails on is a
    /// lint policy nobody follows.
    #[test]
    fn clippy_denies_warnings() {
        let args = clippy_task().args;
        let denied = args.windows(2).any(|w| w == ["-D", "warnings"]);
        assert!(denied, "clippy is not run with -D warnings: {args:?}");
    }
}
