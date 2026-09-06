// SPDX-License-Identifier: Apache-2.0

//! Git fixtures for the worktree tests.

use std::path::Path;
use std::process::Command;

/// Initialise a fresh git repository at `dir` with one (empty) commit and a
/// throwaway local identity, so `git worktree add -b` has a `HEAD` to fork
/// from. Panics on any git failure — a fixture that half-initialised is
/// worse than a loud test.
pub fn init_git_repo(dir: &Path) {
    let run = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(dir)
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(args)
            .status()
            .expect("git command");
        assert!(status.success(), "git {args:?} failed in {}", dir.display());
    };

    run(&["init", "--initial-branch=main"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    // An empty commit so HEAD exists — `git worktree add -b` refuses to
    // fork from a repo with no commits.
    run(&["commit", "--allow-empty", "-m", "init"]);
}
