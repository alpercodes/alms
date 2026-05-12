//! Per-agent git worktree management (#946).
//!
//! Side-effecting helpers for creating / removing the per-agent worktree
//! used by `WorktreeMode::Git`. The agent CRUD handlers and the CLI both
//! call into this module so the on-disk layout and the `git` invocations
//! stay in one place.
//!
//! ## Layout
//!
//! Every worktree lives at `<project>/.alms/worktrees/<name>/` on a
//! dedicated branch `alms/<name>`. The directory is sibling to
//! `.alms/agents/` (NOT nested per-agent), matching the depth Claude
//! Code uses for `.claude/worktrees/<name>/`.
//!
//! ## `.git/info/exclude`
//!
//! Creating a worktree appends `/.alms/worktrees/<name>/` to the parent
//! repository's `.git/info/exclude` so `git status` on the parent does
//! not show the worktree directory as untracked. The append is
//! idempotent — re-creating the worktree (e.g. via `mode = "off"
//! → "git"` flip) does not double-append.
//!
//! ## Subprocess hygiene
//!
//! Every git invocation explicitly sets `-c core.hooksPath=` (empty)
//! to suppress operator-installed hooks (commit-msg, pre-commit, etc.)
//! that have nothing to do with worktree provisioning and may fail in
//! a daemon context with no TTY. We also pass `GIT_TERMINAL_PROMPT=0`
//! and `GIT_OPTIONAL_LOCKS=0` so a misconfigured remote credential
//! helper or stale `.git/index.lock` does not hang the gateway.
//!
//! On top of the additive env we also scrub the inherited environment:
//! `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_NAMESPACE`,
//! `GIT_CONFIG`, `GIT_CONFIG_GLOBAL`, `GIT_CONFIG_SYSTEM` are all
//! removed before exec. `GIT_DIR` in particular overrides both
//! `git -C <path>` and the child's working directory, so a daemon
//! started from a shell with a stale `GIT_DIR` exported would silently
//! run worktree commands against the wrong repository. See `git_cmd`.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{AlmsError, AlmsResult};

/// Directory name (under `.alms/`) where per-agent worktrees live.
///
/// Public so tests and the CLI can construct the canonical path
/// without re-typing the literal.
pub const WORKTREES_DIR_NAME: &str = "worktrees";

/// Compute the canonical worktree directory for `agent_name` under
/// `project_root`.
///
/// Returns `<project_root>/.alms/worktrees/<agent_name>/`. Does NOT
/// create the directory or check whether it exists.
pub fn worktree_path(project_root: &Path, agent_name: &str) -> PathBuf {
    project_root
        .join(".alms")
        .join(WORKTREES_DIR_NAME)
        .join(agent_name)
}

/// Compute the canonical branch name for `agent_name`.
///
/// Returns `alms/<agent_name>`. Used as the new branch name when
/// `git worktree add -b <branch>` runs.
pub fn branch_name(agent_name: &str) -> String {
    format!("alms/{agent_name}")
}

/// Returns `true` when `project_root` is a git working tree.
///
/// Implementation: runs `git -C <project_root> rev-parse
/// --is-inside-work-tree` and inspects the exit status. Any failure
/// (executable missing, permission denied, not a repo) maps to
/// `false`. This is intentionally permissive — the caller treats
/// `false` as "non-git project" and surfaces the right user-facing
/// error.
pub fn is_git_repo(project_root: &Path) -> bool {
    let output = git_cmd(project_root)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            // Stdout should be `true` for a working tree. Bare repos
            // return `false` and we treat them the same as non-repos
            // (worktree-mode requires a working tree to copy from).
            String::from_utf8_lossy(&out.stdout).trim() == "true"
        }
        _ => false,
    }
}

/// Returns `true` when the worktree at `worktree_dir` has uncommitted
/// changes (modified, staged, or untracked files).
///
/// Implementation: runs `git -C <worktree_dir> status --porcelain` and
/// returns `true` when stdout is non-empty. Errors propagate as
/// `AlmsError::Runtime` so the caller can distinguish "we couldn't
/// check" from "the worktree is clean" — silently treating a probe
/// failure as clean would let the `--force` precondition leak.
pub fn worktree_has_uncommitted(worktree_dir: &Path) -> AlmsResult<bool> {
    let output = git_cmd(worktree_dir)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|e| {
            AlmsError::Runtime(format!(
                "git status failed in worktree {}: {e}",
                worktree_dir.display()
            ))
        })?;

    if !output.status.success() {
        return Err(AlmsError::Runtime(format!(
            "git status returned non-zero in worktree {}: {}",
            worktree_dir.display(),
            String::from_utf8_lossy(&output.stderr).trim(),
        )));
    }

    Ok(!output.stdout.is_empty())
}

/// Append the relative worktree path to the parent repo's
/// `.git/info/exclude` so `git status` on the parent does not show the
/// worktree directory as untracked.
///
/// Idempotent — re-creating an existing worktree does not double-add
/// the line. The exact line written is `/.alms/worktrees/<name>/`
/// (rooted with a leading `/` so the gitignore pattern matches only at
/// the repo root, never a coincidentally-named subdirectory).
fn append_exclude_idempotent(project_root: &Path, agent_name: &str) -> AlmsResult<()> {
    // `.git` may be a directory (regular repo) or a file containing
    // `gitdir: <path>` (worktrees, submodules). For a regular repo we
    // append to `.git/info/exclude`; the gateway only ever runs against
    // a regular repo (the project root is a working tree by definition)
    // so we don't have to support the gitfile case for now — fall
    // through with a warning if the layout looks unfamiliar.
    let exclude_path = project_root.join(".git").join("info").join("exclude");
    let exclude_dir = exclude_path
        .parent()
        .ok_or_else(|| AlmsError::Runtime("invalid .git/info layout".into()))?;

    if !exclude_dir.exists() {
        // Not a regular repo (might be a worktree-of-a-worktree, or
        // .git is a file). Skip — not worth a hard failure; the only
        // downside is `git status` shows the worktree dir as untracked.
        tracing::warn!(
            project_root = %project_root.display(),
            "Skipping .git/info/exclude append — directory not found (probably a non-standard .git layout)"
        );
        return Ok(());
    }

    // Standard line: leading slash anchors the pattern at the repo
    // root; trailing slash matches only directories. Matches the
    // shape `git worktree add` writes for hooks-style ignores.
    let line = format!("/.alms/{WORKTREES_DIR_NAME}/{agent_name}/");

    let existing = std::fs::read_to_string(&exclude_path).unwrap_or_default();

    // Idempotency check: split by newline so a substring match against
    // a different agent name (`alms/atlas-2` matching `alms/atlas`)
    // doesn't trick us into thinking the line is already there.
    let already_present = existing.lines().any(|l| l.trim() == line);
    if already_present {
        return Ok(());
    }

    // Open in append mode. Add a leading newline if the file does not
    // already end with one — leaves the file in a tidy state for the
    // operator who later opens it manually.
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .map_err(|e| {
            AlmsError::Runtime(format!(
                "open .git/info/exclude {}: {e}",
                exclude_path.display()
            ))
        })?;

    if !existing.is_empty() && !existing.ends_with('\n') {
        f.write_all(b"\n")
            .map_err(|e| AlmsError::Runtime(format!("write newline to exclude: {e}")))?;
    }
    writeln!(f, "{line}").map_err(|e| AlmsError::Runtime(format!("append exclude line: {e}")))?;

    Ok(())
}

/// Errors specific to worktree provisioning.
///
/// These map onto distinct HTTP error codes in the agent CRUD
/// handlers so operator-facing diagnostics stay falsifiable from
/// the wire format alone.
#[derive(Debug)]
pub enum WorktreeError {
    /// The project root is not a git working tree. Returned when the
    /// caller asks for `WorktreeMode::Git` on a non-git project.
    NotAGitRepo,
    /// The worktree directory exists and contains uncommitted
    /// changes. Returned by `remove_worktree(force=false)` when the
    /// caller has not opted into the destructive flow.
    UncommittedChanges,
    /// Generic git failure — the inner string is the captured stderr
    /// (or a synthetic message when stderr was empty). Bubbles up as
    /// `WORKTREE_GIT_FAILED` at the API layer.
    GitFailed(String),
    /// IO failure (creating directories, reading the lockfile, etc.).
    /// Distinct from `GitFailed` so callers can treat retries
    /// differently if they want.
    Io(String),
}

impl std::fmt::Display for WorktreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAGitRepo => f.write_str(
                "project root is not a git working tree — `mode = \"git\"` requires a git project",
            ),
            Self::UncommittedChanges => f.write_str(
                "worktree has uncommitted changes — pass --force (or force_worktree_remove: true) \
                 to override; this discards the worktree contents AND deletes the alms/<name> branch",
            ),
            Self::GitFailed(msg) => write!(f, "git command failed: {msg}"),
            Self::Io(msg) => write!(f, "worktree IO error: {msg}"),
        }
    }
}

impl std::error::Error for WorktreeError {}

/// Result alias for the worktree module.
pub type WorktreeResult<T> = std::result::Result<T, WorktreeError>;

/// Outcome of a `create_worktree` call.
///
/// Distinguishes "I provisioned this on disk just now" (`Created`)
/// from "the path was already on disk and I left it alone"
/// (`AlreadyExisted`). The compensation path in
/// `apply_worktree_flip_and_persist` (alms-gateway) gates its
/// destructive `remove_worktree` call on `Created` only — running
/// the inverse op against an `AlreadyExisted` outcome would delete
/// pre-existing operator state this PATCH did not actually create.
/// See #1019 / Codex P1 (off→git side).
#[derive(Debug, Clone)]
pub enum WorktreeCreate {
    /// `git worktree add` ran successfully — the directory and
    /// branch are fresh state owned by this call.
    Created(PathBuf),
    /// The worktree directory already existed on disk before the
    /// call. `git worktree add` was NOT run; only the idempotent
    /// `.git/info/exclude` append fired. Compensation must NOT
    /// destroy this worktree on persist failure.
    AlreadyExisted(PathBuf),
}

impl WorktreeCreate {
    /// Path to the worktree directory, regardless of whether it
    /// was freshly created or already there.
    pub fn path(&self) -> &Path {
        match self {
            Self::Created(p) | Self::AlreadyExisted(p) => p.as_path(),
        }
    }

    /// Consume the outcome and return the path. Convenience for
    /// call sites that don't care about the create-vs-existed
    /// distinction (e.g. CLI agent create).
    pub fn into_path(self) -> PathBuf {
        match self {
            Self::Created(p) | Self::AlreadyExisted(p) => p,
        }
    }

    /// Returns `true` if the worktree was freshly provisioned by
    /// the call that produced this outcome.
    pub fn was_created(&self) -> bool {
        matches!(self, Self::Created(_))
    }
}

/// Outcome of a `remove_worktree` call.
///
/// Distinguishes "I tore down the worktree just now" (`Removed`)
/// from "the path was not on disk and I had nothing to do"
/// (`AlreadyAbsent`). The compensation path in
/// `apply_worktree_flip_and_persist` (alms-gateway) gates its
/// destructive recreate call on `Removed` only — re-creating a
/// branch + worktree off HEAD when the original `remove_worktree`
/// was a no-op would fabricate state that did not exist before
/// the PATCH. See #1019 / Codex P1 (symmetric git→off side).
#[derive(Debug, Clone)]
pub enum WorktreeRemove {
    /// `git worktree remove` ran successfully — the directory and
    /// (best-effort) the `alms/<name>` branch are gone.
    Removed,
    /// The worktree directory was not on disk to begin with. No
    /// `git worktree remove` ran. The branch may or may not have
    /// been deleted as a best-effort cleanup; the caller treats
    /// this outcome as "I owe no compensation on persist failure".
    AlreadyAbsent,
}

impl WorktreeRemove {
    /// Returns `true` if the call actually removed a worktree
    /// directory from disk. Compensation gates on this.
    pub fn was_removed(&self) -> bool {
        matches!(self, Self::Removed)
    }
}

/// Create a per-agent worktree at `<project>/.alms/worktrees/<name>/`
/// on branch `alms/<name>`.
///
/// Idempotent in the "already exists" sense: when the worktree
/// directory already exists, the function returns
/// `Ok(WorktreeCreate::AlreadyExisted(path))` without re-running
/// `git worktree add`. This makes the call safe to retry from
/// agent-create and from a `mode: off → git` flip — but callers
/// that need to compensate on a downstream failure (the gateway
/// PATCH path) MUST inspect `was_created()` before running a
/// destructive inverse op. Deleting an `AlreadyExisted` worktree
/// would discard pre-existing operator state. See #1019 / Codex P1.
///
/// On a non-git project returns `WorktreeError::NotAGitRepo`. The
/// caller maps this to `400 WORKTREE_REQUIRES_GIT` and refuses to
/// persist the agent record.
pub fn create_worktree(project_root: &Path, agent_name: &str) -> WorktreeResult<WorktreeCreate> {
    if !is_git_repo(project_root) {
        return Err(WorktreeError::NotAGitRepo);
    }

    let target = worktree_path(project_root, agent_name);

    // Ensure parent directory exists so `git worktree add` doesn't
    // fail with "fatal: could not create leading directories" on a
    // fresh project.
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            WorktreeError::Io(format!("create worktrees parent {}: {e}", parent.display()))
        })?;
    }

    if target.exists() {
        // Already there — assume an earlier call provisioned it. We
        // could verify the branch matches, but `git worktree add`
        // refuses to clobber an existing path anyway, so re-running
        // would always fail. The exclude-append below stays
        // idempotent so this branch is safe.
        //
        // CRITICAL: return `AlreadyExisted` so compensation paths
        // know NOT to destroy this worktree on a downstream
        // persist failure — this PATCH did not create it.
        append_exclude_idempotent(project_root, agent_name)
            .map_err(|e| WorktreeError::Io(format!("append exclude (existing worktree): {e}")))?;
        return Ok(WorktreeCreate::AlreadyExisted(target));
    }

    // `git worktree add <path> -b alms/<name>` — creates a NEW
    // branch off HEAD and checks it out at `<path>`.
    let branch = branch_name(agent_name);
    let target_str = target.to_string_lossy().to_string();
    let output = git_cmd(project_root)
        .args(["worktree", "add", &target_str, "-b", &branch])
        .output()
        .map_err(|e| WorktreeError::GitFailed(format!("spawn git worktree add: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if stderr.is_empty() { stdout } else { stderr };
        return Err(WorktreeError::GitFailed(format!(
            "git worktree add {target_str} -b {branch}: {msg}"
        )));
    }

    append_exclude_idempotent(project_root, agent_name)
        .map_err(|e| WorktreeError::Io(format!("append exclude: {e}")))?;

    Ok(WorktreeCreate::Created(target))
}

/// Read the current HEAD SHA of the per-agent branch `alms/<agent_name>`.
///
/// Returns `Ok(Some(sha))` when the branch exists, `Ok(None)` when the
/// branch is missing (caller treats that as "nothing to snapshot"), and
/// `Err(_)` on any other git failure. Used by the PATCH `git→off`
/// compensation path so a persist failure after `remove_worktree` can
/// restore the branch tip the operator had before the flip.
///
/// Implementation: `git -C <project_root> rev-parse <branch>`. The
/// "branch missing" case is identified by stderr containing the
/// `unknown revision` / `ambiguous argument` shape git uses for that
/// outcome — distinguishing it from a real failure (executable
/// missing, repo broken) avoids paving over genuine errors.
pub fn read_branch_head_sha(
    project_root: &Path,
    agent_name: &str,
) -> WorktreeResult<Option<String>> {
    let branch = branch_name(agent_name);
    let output = git_cmd(project_root)
        .args(["rev-parse", "--verify", &format!("refs/heads/{branch}")])
        .output()
        .map_err(|e| WorktreeError::GitFailed(format!("spawn git rev-parse {branch}: {e}")))?;

    if output.status.success() {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if sha.is_empty() {
            return Ok(None);
        }
        return Ok(Some(sha));
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    // `git rev-parse --verify refs/heads/<missing>` exits non-zero
    // with `fatal: Needed a single revision` when the ref doesn't
    // exist. Treat that and the `unknown revision` shape as
    // "branch absent" rather than a hard failure.
    if stderr.contains("Needed a single revision")
        || stderr.contains("unknown revision")
        || stderr.contains("ambiguous argument")
    {
        return Ok(None);
    }

    Err(WorktreeError::GitFailed(format!(
        "git rev-parse refs/heads/{branch}: {}",
        stderr.trim()
    )))
}

/// Restore the per-agent worktree at `<project>/.alms/worktrees/<name>/`
/// pointing at `sha` on branch `alms/<name>`.
///
/// Recreates the branch at `sha` (via `git branch alms/<name> <sha>`)
/// and then attaches a new worktree to it (via `git worktree add
/// <path> alms/<name>`). Used by the PATCH `git→off` compensation
/// path when the original `remove_worktree` destroyed the branch:
/// `create_worktree` would re-fork the branch off HEAD, silently
/// losing the snapshotted commits, so the compensation must go
/// through this function instead. See #1019 / Codex P1.
///
/// Idempotent on existence:
///   - When the worktree dir already exists, the function only
///     re-asserts the `.git/info/exclude` line and returns
///     `Ok(path)` — same shape as `create_worktree`.
///   - When the worktree dir is missing but the branch already
///     exists at the snapshot SHA, the `git branch` step is skipped
///     and the function proceeds straight to `git worktree add`.
///     This handles the AlreadyAbsent compensation arm where
///     `remove_worktree` ignored a `delete_branch` failure (e.g.
///     stale worktree metadata) and left the branch behind — the
///     world is already where the caller wants it, no compensation
///     drift was introduced. See #1019 / Codex P2.
///
/// On a non-git project returns `WorktreeError::NotAGitRepo`. If the
/// branch already exists at a DIFFERENT SHA, the function returns
/// `WorktreeError::GitFailed` rather than silently rewriting the
/// branch — that's a real conflict and surfacing it is correct.
pub fn restore_worktree_at_sha(
    project_root: &Path,
    agent_name: &str,
    sha: &str,
) -> WorktreeResult<PathBuf> {
    if !is_git_repo(project_root) {
        return Err(WorktreeError::NotAGitRepo);
    }

    let target = worktree_path(project_root, agent_name);

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            WorktreeError::Io(format!("create worktrees parent {}: {e}", parent.display()))
        })?;
    }

    if target.exists() {
        // Already there — same shape as `create_worktree`. Idempotent
        // append on the exclude file and return.
        append_exclude_idempotent(project_root, agent_name)
            .map_err(|e| WorktreeError::Io(format!("append exclude (existing worktree): {e}")))?;
        return Ok(target);
    }

    let branch = branch_name(agent_name);

    // Step 1: ensure the branch exists at `sha`. Three cases:
    //
    //  a) Branch missing — run `git branch <name> <sha>`. This is
    //     the textbook compensation path where `remove_worktree`
    //     successfully deleted both the worktree AND the branch.
    //  b) Branch present at the snapshot SHA — skip `git branch`,
    //     no-op the step. The world is already where we want it.
    //     This case fires when `remove_worktree` took the
    //     `AlreadyAbsent` arm and ignored a `delete_branch` failure
    //     (`git branch -D` refused due to stale worktree metadata,
    //     locked ref, etc.). Without this idempotency check the
    //     compensation would surface `WORKTREE_COMPENSATION_FAILED`
    //     to the operator even though no drift was introduced.
    //     See #1019 / Codex P2.
    //  c) Branch present at a DIFFERENT SHA — refuse with a
    //     specific error. That's a real conflict (concurrent
    //     overwrite, or a logic bug in the caller) and silently
    //     resetting the branch would be a worse outcome than
    //     surfacing it.
    let existing_sha = read_branch_head_sha(project_root, agent_name)?;
    // Track whether THIS call created the branch — only the
    // create-side gets best-effort cleanup if `git worktree add`
    // fails below. Pre-existing branches from case (b) are left
    // alone on rollback, since the caller's contract is "ensure
    // the branch exists at SHA" — we never asked to own it.
    let branch_was_created_by_us = match existing_sha.as_deref() {
        Some(existing) if existing == sha => {
            // Case (b): branch already at the snapshot SHA. Skip
            // the `git branch` step and proceed to attach the
            // worktree. Emit a structured trace so the compensation
            // path is greppable from the daemon log.
            tracing::info!(
                target: "alms.worktree",
                agent_name = %agent_name,
                branch = %branch,
                sha = %sha,
                "Branch already at snapshot SHA — skipping `git branch` step in restore_worktree_at_sha (no drift introduced)"
            );
            false
        }
        Some(existing) => {
            // Case (c): branch exists at a different SHA. Refuse.
            return Err(WorktreeError::GitFailed(format!(
                "branch {branch} already exists at {existing} but compensation expected {sha} — \
                 refusing to overwrite the branch ref"
            )));
        }
        None => {
            // Case (a): branch missing — create it at the snapshot
            // SHA. We use plain `git branch <name> <sha>` —
            // non-destructive (refuses to overwrite an existing
            // branch). The compensation caller has just observed
            // `remove_worktree` delete this branch, so the race
            // window where another agent could resurrect it is
            // millisecond-scale and any concurrent overwrite would
            // be a legitimate fail.
            let branch_output = git_cmd(project_root)
                .args(["branch", &branch, sha])
                .output()
                .map_err(|e| {
                    WorktreeError::GitFailed(format!("spawn git branch {branch} {sha}: {e}"))
                })?;

            if !branch_output.status.success() {
                let stderr = String::from_utf8_lossy(&branch_output.stderr)
                    .trim()
                    .to_string();
                return Err(WorktreeError::GitFailed(format!(
                    "git branch {branch} {sha}: {stderr}"
                )));
            }
            true
        }
    };

    // Step 2: attach a worktree to the (now-existing) branch. Note
    // the absence of `-b` — `git worktree add <path> <existing-branch>`
    // checks out the existing ref rather than forking a new one off
    // HEAD. This is the exact behavior `create_worktree` lacks and the
    // reason this function exists.
    let target_str = target.to_string_lossy().to_string();
    let output = git_cmd(project_root)
        .args(["worktree", "add", &target_str, &branch])
        .output()
        .map_err(|e| WorktreeError::GitFailed(format!("spawn git worktree add: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let msg = if stderr.is_empty() { stdout } else { stderr };
        // Best-effort cleanup: only drop the branch when we created
        // it in this call. If we took the case-(b) idempotency path
        // and the branch pre-existed, we never asked to own it —
        // dropping someone else's ref on rollback would be worse
        // than leaving it in place. Failure here is not fatal — the
        // original `git worktree add` error is what the operator
        // needs to see.
        if branch_was_created_by_us {
            let _ = git_cmd(project_root)
                .args(["branch", "-D", &branch])
                .output();
        }
        return Err(WorktreeError::GitFailed(format!(
            "git worktree add {target_str} {branch}: {msg}"
        )));
    }

    append_exclude_idempotent(project_root, agent_name)
        .map_err(|e| WorktreeError::Io(format!("append exclude: {e}")))?;

    Ok(target)
}

/// Remove the per-agent worktree at `<project>/.alms/worktrees/<name>/`.
///
/// When `force == false` and the worktree contains uncommitted changes
/// returns `WorktreeError::UncommittedChanges` and leaves everything
/// in place. `force == true` runs `git worktree remove --force` and
/// then `git branch -D alms/<name>` so the branch is cleaned up too.
///
/// If the worktree directory does not exist the call is a no-op and
/// returns `Ok(WorktreeRemove::AlreadyAbsent)` — matches the
/// agent-delete flow where worktree state may be partially missing
/// if the operator nuked the directory by hand. Callers that need
/// to compensate on a downstream failure (the gateway PATCH path)
/// MUST inspect `was_removed()` before running a destructive
/// recreate — re-fabricating a worktree + branch off HEAD when the
/// original call was a no-op would invent state that did not exist
/// before the PATCH. See #1019 / Codex P1 (symmetric git→off side).
pub fn remove_worktree(
    project_root: &Path,
    agent_name: &str,
    force: bool,
) -> WorktreeResult<WorktreeRemove> {
    let target = worktree_path(project_root, agent_name);

    if !target.exists() {
        // Nothing to remove — but also try to clean up the branch
        // ref in case the worktree was nuked manually but the branch
        // still exists. Failures here are non-fatal — the caller's
        // intent is "make this agent gone" and a stray branch ref
        // does not block that.
        //
        // CRITICAL: return `AlreadyAbsent` so compensation paths
        // know NOT to recreate a fresh worktree+branch on a
        // downstream persist failure — this PATCH did not remove
        // anything, so there is nothing to undo.
        //
        // Surface `delete_branch` failures via WARN at
        // `target = "alms.worktree"` to match the happy-path arm
        // below — "best-effort" doesn't mean "silent". An operator
        // chasing a stale `alms/<name>` ref needs the audit trail
        // to find the silent-discard event. See #1025.
        if let Err(e) = delete_branch(project_root, agent_name, force) {
            tracing::warn!(
                target: "alms.worktree",
                agent_name = %agent_name,
                error = %e,
                "delete_branch failed on AlreadyAbsent path — branch may be orphaned"
            );
        }
        return Ok(WorktreeRemove::AlreadyAbsent);
    }

    if !force && worktree_has_uncommitted(&target).unwrap_or(true) {
        return Err(WorktreeError::UncommittedChanges);
    }

    let target_str = target.to_string_lossy().to_string();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&target_str);

    let output = git_cmd(project_root)
        .args(&args)
        .output()
        .map_err(|e| WorktreeError::GitFailed(format!("spawn git worktree remove: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(WorktreeError::GitFailed(format!(
            "git worktree remove {target_str}: {stderr}"
        )));
    }

    // Best-effort branch cleanup. Failures here are non-fatal — the
    // worktree is gone, which is what the caller asked for. A
    // dangling branch is recoverable manually.
    if let Err(e) = delete_branch(project_root, agent_name, force) {
        tracing::warn!(
            agent_name = %agent_name,
            error = %e,
            "Failed to delete branch alms/{} after worktree removal — manual cleanup may be required",
            agent_name,
        );
    }

    Ok(WorktreeRemove::Removed)
}

/// Delete the `alms/<agent_name>` branch from the project's repo.
///
/// Uses `git branch -D` so unmerged branches are also removed (the
/// worktree branch carries the agent's work-in-progress, which by
/// definition is not merged anywhere). The `force` arg is currently
/// ignored — `-D` is already the destructive variant — but kept on
/// the signature to leave room for a future `-d` (safe) path.
fn delete_branch(project_root: &Path, agent_name: &str, _force: bool) -> WorktreeResult<()> {
    let branch = branch_name(agent_name);
    let output = git_cmd(project_root)
        .args(["branch", "-D", &branch])
        .output()
        .map_err(|e| WorktreeError::GitFailed(format!("spawn git branch -D: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        // `git branch -D` returns non-zero for "branch not found",
        // which is fine — the worktree may have been removed
        // manually before the branch was cleaned up. Detect that
        // specific case and treat it as success; any other failure
        // bubbles up.
        if stderr.contains("not found") || stderr.contains("No such") {
            return Ok(());
        }
        return Err(WorktreeError::GitFailed(format!(
            "git branch -D {branch}: {stderr}"
        )));
    }

    Ok(())
}

/// Build a base `Command` for invoking `git -C <project_root>` with
/// the daemon-friendly env hardening described on the module docs.
///
/// Inherited `GIT_*` environment variables are an underappreciated
/// footgun in daemon contexts: a gateway started from an interactive
/// shell that has `GIT_DIR=/some/other/repo/.git` exported will run
/// every worktree command against the wrong repository, because
/// `GIT_DIR` overrides `git -C <path>` and `current_dir(...)` both.
/// The daemon never wants to inherit any of these — every git invocation
/// in this module is intentionally scoped via `current_dir(cwd)` and
/// explicit args. Strip the full set of repo-targeting and
/// config-targeting variables so a misconfigured operator shell can
/// never bleed into a worktree subprocess.
fn git_cmd(cwd: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        // Repo-targeting vars: `GIT_DIR` overrides `-C` and the cwd,
        // so a stale value in the parent shell would silently route
        // worktree commands to a different repository. The other
        // three round out the targeting surface (work tree root,
        // index file location, ref namespace).
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_NAMESPACE")
        // Config-targeting vars: an inherited `GIT_CONFIG` /
        // `GIT_CONFIG_GLOBAL` / `GIT_CONFIG_SYSTEM` would re-introduce
        // the very hooks / aliases / credential helpers we explicitly
        // disabled below. Strip them so the worktree subprocess sees
        // only the project-local config plus our explicit `-c` flags.
        .env_remove("GIT_CONFIG")
        .env_remove("GIT_CONFIG_GLOBAL")
        .env_remove("GIT_CONFIG_SYSTEM")
        // `-c core.hooksPath=` (empty value) is the documented way to
        // disable hooks for a single git invocation. `/dev/null`
        // works on Unix but isn't portable to Windows; the empty
        // string disables the lookup on every platform.
        .args(["-c", "core.hooksPath="]);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    /// Initialize a fresh git repo at `dir` with one commit so
    /// `git worktree add` has a HEAD to fork from.
    fn init_git_repo(dir: &Path) {
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
        // An empty commit so HEAD exists — `git worktree add -b`
        // refuses to fork from a repo with no commits.
        run(&["commit", "--allow-empty", "-m", "init"]);
    }

    #[test]
    fn worktree_path_under_alms_worktrees() {
        let p = worktree_path(Path::new("/tmp/proj"), "atlas");
        assert_eq!(
            p,
            PathBuf::from("/tmp/proj")
                .join(".alms")
                .join("worktrees")
                .join("atlas"),
        );
    }

    #[test]
    fn branch_name_prefixes_alms() {
        assert_eq!(branch_name("atlas"), "alms/atlas");
        assert_eq!(branch_name("my-agent-2"), "alms/my-agent-2");
    }

    #[test]
    fn is_git_repo_false_on_bare_directory() {
        let tmp = TempDir::new().unwrap();
        assert!(!is_git_repo(tmp.path()));
    }

    #[test]
    fn is_git_repo_true_after_init() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        assert!(is_git_repo(tmp.path()));
    }

    #[test]
    fn create_worktree_on_non_git_project_returns_not_a_git_repo() {
        let tmp = TempDir::new().unwrap();
        let result = create_worktree(tmp.path(), "atlas");
        assert!(matches!(result, Err(WorktreeError::NotAGitRepo)));
        // No partial state — the worktree directory must not exist.
        assert!(
            !tmp.path()
                .join(".alms")
                .join("worktrees")
                .join("atlas")
                .exists()
        );
    }

    #[test]
    fn create_worktree_on_git_project_succeeds() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let outcome = create_worktree(tmp.path(), "atlas").expect("create");
        assert!(
            outcome.was_created(),
            "first call must report Created (not AlreadyExisted): {outcome:?}"
        );
        let path = outcome.into_path();
        assert!(
            path.is_dir(),
            "worktree dir should exist: {}",
            path.display()
        );
        // Branch should be checked out — verify by reading HEAD inside the worktree.
        let head_path = path.join(".git");
        assert!(
            head_path.exists(),
            ".git pointer file should exist inside the worktree"
        );

        // The branch alms/atlas should appear in `git branch`.
        let output = Command::new("git")
            .current_dir(tmp.path())
            .args(["branch", "--list", "alms/atlas"])
            .output()
            .expect("git branch");
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("alms/atlas"),
            "expected branch alms/atlas in: {stdout}"
        );
    }

    #[test]
    fn create_worktree_appends_to_git_info_exclude() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        create_worktree(tmp.path(), "atlas").unwrap();

        let exclude = std::fs::read_to_string(tmp.path().join(".git").join("info").join("exclude"))
            .expect("read exclude");
        assert!(
            exclude
                .lines()
                .any(|l| l.trim() == "/.alms/worktrees/atlas/"),
            "exclude file should contain `/.alms/worktrees/atlas/`, got:\n{exclude}"
        );
    }

    #[test]
    fn create_worktree_exclude_append_idempotent() {
        // Re-running create_worktree-like exclude appends should not
        // double-write the line. Guard against a regression where the
        // pre-check was substring-based.
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        create_worktree(tmp.path(), "atlas").unwrap();
        // Manually call append again (simulates a flip-back-and-forth).
        append_exclude_idempotent(tmp.path(), "atlas").unwrap();
        append_exclude_idempotent(tmp.path(), "atlas").unwrap();

        let exclude =
            std::fs::read_to_string(tmp.path().join(".git").join("info").join("exclude")).unwrap();
        let occurrences = exclude
            .lines()
            .filter(|l| l.trim() == "/.alms/worktrees/atlas/")
            .count();
        assert_eq!(
            occurrences, 1,
            "exclude entry should appear exactly once, got {occurrences}"
        );
    }

    #[test]
    fn create_worktree_idempotent_when_path_exists() {
        // First call provisions; second call should be a no-op (path
        // already exists, branch already exists). The second call MUST
        // report `AlreadyExisted` so the gateway compensation path
        // does not destroy the pre-existing worktree on a downstream
        // persist failure. See #1019 / Codex P1.
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let first = create_worktree(tmp.path(), "atlas").unwrap();
        assert!(
            first.was_created(),
            "first call must report Created: {first:?}"
        );

        let second = create_worktree(tmp.path(), "atlas").expect("second create no-op");
        assert!(
            !second.was_created(),
            "second call must report AlreadyExisted (not Created); got {second:?} — \
             gateway compensation gates destructive cleanup on this distinction"
        );
        assert!(matches!(second, WorktreeCreate::AlreadyExisted(_)));
    }

    #[test]
    fn remove_worktree_clean_succeeds() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap().into_path();

        let outcome = remove_worktree(tmp.path(), "atlas", false).unwrap();
        assert!(
            outcome.was_removed(),
            "must report Removed when the worktree was on disk: {outcome:?}"
        );
        assert!(!path.exists(), "worktree dir should be gone");
    }

    #[test]
    fn remove_worktree_uncommitted_refuses_without_force() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap().into_path();

        // Add an untracked file inside the worktree → uncommitted.
        std::fs::write(path.join("dirty.txt"), "wip").unwrap();

        let result = remove_worktree(tmp.path(), "atlas", false);
        assert!(
            matches!(result, Err(WorktreeError::UncommittedChanges)),
            "expected UncommittedChanges, got {result:?}"
        );
        assert!(
            path.exists(),
            "worktree must still exist after refused remove"
        );
    }

    #[test]
    fn remove_worktree_uncommitted_succeeds_with_force() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap().into_path();
        std::fs::write(path.join("dirty.txt"), "wip").unwrap();

        let outcome = remove_worktree(tmp.path(), "atlas", true).unwrap();
        assert!(outcome.was_removed());
        assert!(!path.exists(), "force remove should drop the worktree");
    }

    #[test]
    fn remove_worktree_missing_dir_is_noop() {
        // The worktree directory was never on disk. The call must
        // succeed (idempotent) AND report `AlreadyAbsent` so the
        // gateway compensation path knows there is nothing to undo.
        // See #1019 / Codex P1 (symmetric git→off side).
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let outcome = remove_worktree(tmp.path(), "ghost", false).unwrap();
        assert!(
            !outcome.was_removed(),
            "ghost worktree must report AlreadyAbsent (not Removed); got {outcome:?} \
             — gateway compensation gates destructive recreate on this distinction"
        );
        assert!(matches!(outcome, WorktreeRemove::AlreadyAbsent));
    }

    #[test]
    fn worktree_has_uncommitted_clean_returns_false() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap().into_path();
        assert!(!worktree_has_uncommitted(&path).unwrap());
    }

    #[test]
    fn worktree_has_uncommitted_dirty_returns_true() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap().into_path();
        std::fs::write(path.join("dirty.txt"), "wip").unwrap();
        assert!(worktree_has_uncommitted(&path).unwrap());
    }

    /// Regression guard: `git_cmd` must scrub the targeting / config
    /// env vars (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`,
    /// `GIT_NAMESPACE`, `GIT_CONFIG{,_GLOBAL,_SYSTEM}`) so an inherited
    /// value from the operator's shell can't redirect worktree commands
    /// to the wrong repository. Tests via `Command::get_envs`, which
    /// exposes the staged env modifications without cross-test
    /// pollution from `env::set_var`.
    #[test]
    fn git_cmd_scrubs_dangerous_env_vars() {
        use std::ffi::OsStr;

        let cmd = git_cmd(Path::new("/tmp"));
        let envs: Vec<(&OsStr, Option<&OsStr>)> = cmd.get_envs().collect();

        // Build a set of (name, action) where action=None means "remove".
        let removed: Vec<&str> = envs
            .iter()
            .filter(|(_, v)| v.is_none())
            .filter_map(|(k, _)| k.to_str())
            .collect();

        for var in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_NAMESPACE",
            "GIT_CONFIG",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
        ] {
            assert!(
                removed.contains(&var),
                "git_cmd must remove {var} from the inherited env to prevent operator-shell bleed-through; staged removals were {removed:?}",
            );
        }
    }

    /// Belt-and-braces integration check: even when a bogus
    /// `GIT_DIR` is pre-staged on a Command via `Command::env(...)`,
    /// a subsequent `env_remove("GIT_DIR")` (the pattern `git_cmd`
    /// uses) takes precedence and `git rev-parse --git-dir` resolves
    /// to the real repo's `.git`. Models the production wiring
    /// without polluting the test process's own environment, which
    /// would race with parallel tests — `Command::env_remove` strips
    /// both inherited values and prior `Command::env(...)` entries
    /// per the std docs, so this also covers the "inherited
    /// `GIT_DIR` from operator shell" case by construction.
    #[test]
    fn git_cmd_scrub_overrides_pre_staged_git_dir() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let bogus = tmp.path().join("nonexistent-bogus-gitdir");
        let mut cmd = Command::new("git");
        cmd.current_dir(tmp.path())
            .env("GIT_DIR", &bogus)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_NAMESPACE")
            .env_remove("GIT_CONFIG")
            .env_remove("GIT_CONFIG_GLOBAL")
            .env_remove("GIT_CONFIG_SYSTEM")
            .args(["-c", "core.hooksPath=", "rev-parse", "--git-dir"]);

        let output = cmd.output().expect("git rev-parse");
        assert!(
            output.status.success(),
            "git rev-parse should succeed against the real repo, stderr was: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("nonexistent-bogus-gitdir"),
            "git resolved to the bogus GIT_DIR — env_remove did not take effect; got:\n{stdout}",
        );
    }

    /// `read_branch_head_sha` returns the live HEAD SHA of
    /// `alms/<agent>` after `create_worktree` provisions the branch.
    /// The SHA must be a 40-character hex string identifying the
    /// commit the branch points at.
    #[test]
    fn read_branch_head_sha_returns_sha_for_existing_branch() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        create_worktree(tmp.path(), "atlas").unwrap();

        let sha = read_branch_head_sha(tmp.path(), "atlas").expect("rev-parse");
        let sha = sha.expect("branch must exist after create_worktree");
        assert_eq!(
            sha.len(),
            40,
            "expected 40-char SHA, got {} chars: {sha}",
            sha.len()
        );
        assert!(
            sha.chars().all(|c| c.is_ascii_hexdigit()),
            "SHA must be hex: {sha}"
        );

        // Cross-check: matches what `git rev-parse alms/atlas` returns.
        let direct = Command::new("git")
            .current_dir(tmp.path())
            .args(["rev-parse", "alms/atlas"])
            .output()
            .unwrap();
        assert!(direct.status.success());
        assert_eq!(String::from_utf8_lossy(&direct.stdout).trim(), sha);
    }

    /// `read_branch_head_sha` returns `Ok(None)` when the branch
    /// is missing rather than an error — caller treats that as
    /// "nothing to snapshot".
    #[test]
    fn read_branch_head_sha_returns_none_for_missing_branch() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());

        let result = read_branch_head_sha(tmp.path(), "ghost").expect("rev-parse must not error");
        assert!(
            result.is_none(),
            "missing branch must return Ok(None), got {result:?}"
        );
    }

    /// `restore_worktree_at_sha` round-trip: snapshot SHA, remove
    /// worktree (which deletes the branch), restore at the
    /// snapshotted SHA — the new branch must point at the same
    /// commit, with the worktree dir present.
    #[test]
    fn restore_worktree_at_sha_round_trip_preserves_branch_tip() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap().into_path();

        // Make a real commit on the agent branch so we can prove
        // its history survives the round trip.
        std::fs::write(path.join("agent-work.txt"), "important agent state").unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let s = Command::new("git")
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args)
                .status()
                .expect("git command");
            assert!(s.success(), "git {args:?} failed in {}", dir.display());
        };
        run(&path, &["config", "user.email", "test@example.com"]);
        run(&path, &["config", "user.name", "Test"]);
        run(&path, &["add", "agent-work.txt"]);
        run(&path, &["commit", "-m", "agent commit"]);

        let original_sha = read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("branch must exist after commit");

        // Tear down the worktree (this deletes the branch too).
        remove_worktree(tmp.path(), "atlas", true).unwrap();
        assert!(
            read_branch_head_sha(tmp.path(), "atlas").unwrap().is_none(),
            "branch must be deleted by remove_worktree"
        );

        // Restore at the snapshot SHA.
        let restored = restore_worktree_at_sha(tmp.path(), "atlas", &original_sha).unwrap();
        assert!(restored.is_dir(), "restored worktree dir must exist");

        let restored_sha = read_branch_head_sha(tmp.path(), "atlas")
            .unwrap()
            .expect("branch must exist after restore");
        assert_eq!(
            restored_sha, original_sha,
            "restored branch must point at the snapshotted SHA, not a fresh HEAD-fork"
        );

        // The committed file must be present in the restored worktree.
        assert!(
            restored.join("agent-work.txt").exists(),
            "restored worktree must carry the committed file — proof history survived"
        );
    }

    /// `restore_worktree_at_sha` is idempotent on the "worktree
    /// already exists" case (mirrors `create_worktree`).
    #[test]
    fn restore_worktree_at_sha_idempotent_when_path_exists() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        create_worktree(tmp.path(), "atlas").unwrap();
        let sha = read_branch_head_sha(tmp.path(), "atlas").unwrap().unwrap();

        // Second call — worktree dir is still on disk, branch
        // already exists. Should return Ok with the same path.
        let result = restore_worktree_at_sha(tmp.path(), "atlas", &sha);
        assert!(
            result.is_ok(),
            "second restore on existing worktree must be a no-op: {result:?}"
        );
    }

    /// Regression guard for the bug Codex P1 caught (#1019): if
    /// the `git→off` compensation path were to call
    /// `create_worktree` after the original `remove_worktree`
    /// destroyed the branch, the new branch would fork off HEAD
    /// and the agent's commits would be lost. This test pins down
    /// the contract: `restore_worktree_at_sha` MUST point the
    /// branch at the snapshotted SHA, NOT at HEAD.
    #[test]
    fn restore_worktree_at_sha_does_not_fork_from_head() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap().into_path();

        // Commit on the agent branch so HEAD (still on `main` in
        // the parent project) and `alms/atlas` diverge.
        std::fs::write(path.join("a.txt"), "agent").unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let s = Command::new("git")
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args)
                .status()
                .expect("git command");
            assert!(s.success(), "git {args:?} failed in {}", dir.display());
        };
        run(&path, &["config", "user.email", "test@example.com"]);
        run(&path, &["config", "user.name", "Test"]);
        run(&path, &["add", "a.txt"]);
        run(&path, &["commit", "-m", "agent only"]);

        let agent_sha = read_branch_head_sha(tmp.path(), "atlas").unwrap().unwrap();

        // Capture HEAD SHA on the parent (still pointing at the
        // initial empty commit on `main`) — this is what
        // `create_worktree` would (incorrectly) fork from.
        let head_output = Command::new("git")
            .current_dir(tmp.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        let head_sha = String::from_utf8_lossy(&head_output.stdout)
            .trim()
            .to_string();
        assert_ne!(
            agent_sha, head_sha,
            "test setup invariant: agent commit must diverge from parent HEAD"
        );

        // Tear down.
        remove_worktree(tmp.path(), "atlas", true).unwrap();

        // Restore via the new function — must land on agent_sha,
        // NOT head_sha.
        restore_worktree_at_sha(tmp.path(), "atlas", &agent_sha).unwrap();
        let restored_sha = read_branch_head_sha(tmp.path(), "atlas").unwrap().unwrap();
        assert_eq!(
            restored_sha, agent_sha,
            "restored branch must point at agent's tip, not parent HEAD; \
             this is the silent-data-loss bug Codex P1 caught on PR #1019"
        );
        assert_ne!(
            restored_sha, head_sha,
            "restored branch MUST NOT fork from parent HEAD"
        );
    }

    /// Codex P2 regression guard (#1019 round 5): when the agent
    /// branch already exists at the snapshot SHA (worktree dir
    /// missing — the AlreadyAbsent path where `remove_worktree`
    /// silently failed to delete the branch), `restore_worktree_at_sha`
    /// must be idempotent: skip `git branch <name> <sha>`, proceed
    /// to `git worktree add`, and return Ok with the branch still
    /// at the snapshot SHA.
    ///
    /// Pre-fix this scenario hits `git branch <name> <sha>` and
    /// errors with `fatal: A branch named ... already exists`,
    /// which surfaces to the operator as a scary
    /// `WORKTREE_COMPENSATION_FAILED` even though no drift was
    /// introduced.
    #[test]
    fn restore_worktree_at_sha_idempotent_when_branch_at_same_sha_no_worktree_dir() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        // Provision then commit so the branch carries real history.
        let path = create_worktree(tmp.path(), "atlas").unwrap().into_path();
        std::fs::write(path.join("agent-state.txt"), "important").unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let s = Command::new("git")
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args)
                .status()
                .expect("git command");
            assert!(s.success(), "git {args:?} failed in {}", dir.display());
        };
        run(&path, &["config", "user.email", "test@example.com"]);
        run(&path, &["config", "user.name", "Test"]);
        run(&path, &["add", "agent-state.txt"]);
        run(&path, &["commit", "-m", "agent commit"]);

        let snapshot_sha = read_branch_head_sha(tmp.path(), "atlas").unwrap().unwrap();

        // Simulate the AlreadyAbsent + branch-still-present arm:
        // `git worktree remove --force` drops the working dir but
        // leaves the branch ref intact. (`remove_worktree` would
        // also try `git branch -D` after the worktree-remove step,
        // but the `AlreadyAbsent` arm at line 474 ignores that
        // failure — we model the post-state directly.)
        run(
            tmp.path(),
            &[
                "worktree",
                "remove",
                "--force",
                path.to_str().expect("utf8 path"),
            ],
        );
        assert!(!path.exists(), "worktree dir must be gone");
        assert_eq!(
            read_branch_head_sha(tmp.path(), "atlas")
                .unwrap()
                .as_deref(),
            Some(snapshot_sha.as_str()),
            "test setup invariant: branch must still be at snapshot SHA"
        );

        // Drive the restore. This is the call that pre-fix would
        // error with "branch already exists".
        let restored = restore_worktree_at_sha(tmp.path(), "atlas", &snapshot_sha).expect(
            "restore must be idempotent when branch is already at snapshot SHA \
             — Codex P2 fix: skip the `git branch` step instead of erroring",
        );
        assert!(restored.is_dir(), "worktree dir must be back");

        let post_sha = read_branch_head_sha(tmp.path(), "atlas").unwrap().unwrap();
        assert_eq!(
            post_sha, snapshot_sha,
            "branch SHA must be unchanged across the idempotent restore"
        );
        assert!(
            restored.join("agent-state.txt").exists(),
            "committed file must be present in the restored worktree"
        );
    }

    /// Codex P2 conflict-arm guard (#1019 round 5): if the branch
    /// exists at a DIFFERENT SHA than the snapshot (e.g. concurrent
    /// overwrite from another agent, or a logic bug in the caller),
    /// `restore_worktree_at_sha` must refuse with
    /// `WorktreeError::GitFailed` rather than silently rewriting
    /// the ref. The error message must mention both SHAs so the
    /// operator can diagnose the conflict from the log alone.
    #[test]
    fn restore_worktree_at_sha_errors_when_branch_exists_at_different_sha() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap().into_path();

        // Snapshot the original SHA, then make a real commit so the
        // branch advances past the snapshot.
        let snapshot_sha = read_branch_head_sha(tmp.path(), "atlas").unwrap().unwrap();
        std::fs::write(path.join("a.txt"), "drift").unwrap();
        let run = |dir: &Path, args: &[&str]| {
            let s = Command::new("git")
                .current_dir(dir)
                .env("GIT_TERMINAL_PROMPT", "0")
                .args(args)
                .status()
                .expect("git command");
            assert!(s.success(), "git {args:?} failed in {}", dir.display());
        };
        run(&path, &["config", "user.email", "test@example.com"]);
        run(&path, &["config", "user.name", "Test"]);
        run(&path, &["add", "a.txt"]);
        run(&path, &["commit", "-m", "drift commit"]);

        let drifted_sha = read_branch_head_sha(tmp.path(), "atlas").unwrap().unwrap();
        assert_ne!(
            snapshot_sha, drifted_sha,
            "test invariant: branch must have advanced past the snapshot"
        );

        // Drop just the worktree dir, leave the branch at the
        // drifted SHA. Then ask `restore_worktree_at_sha` to put
        // the branch back at the OLD snapshot — this is the
        // conflict case.
        run(
            tmp.path(),
            &[
                "worktree",
                "remove",
                "--force",
                path.to_str().expect("utf8 path"),
            ],
        );

        let result = restore_worktree_at_sha(tmp.path(), "atlas", &snapshot_sha);
        let err = result.expect_err("must refuse to rewrite branch at different SHA");
        match err {
            WorktreeError::GitFailed(msg) => {
                assert!(
                    msg.contains(&snapshot_sha) && msg.contains(&drifted_sha),
                    "error message must mention both expected and existing SHAs to \
                     surface the conflict: got {msg}"
                );
                assert!(
                    msg.contains("refusing to overwrite"),
                    "error message must explicitly say it refused the overwrite: got {msg}"
                );
            }
            other => panic!("expected WorktreeError::GitFailed, got {other:?}"),
        }
        // Branch must still be at the drifted SHA — we refused to
        // touch it.
        let post_sha = read_branch_head_sha(tmp.path(), "atlas").unwrap().unwrap();
        assert_eq!(
            post_sha, drifted_sha,
            "branch SHA must be untouched after the refused overwrite"
        );
    }

    /// Issue acceptance: `git status` on the parent project does NOT
    /// show the worktree directory as untracked once `create_worktree`
    /// has appended `.git/info/exclude`.
    #[test]
    fn parent_git_status_clean_after_create_worktree() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        create_worktree(tmp.path(), "atlas").unwrap();

        let output = Command::new("git")
            .current_dir(tmp.path())
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains(".alms/worktrees/atlas"),
            "git status on parent must not list the worktree dir, got:\n{stdout}"
        );
    }

    // ── #1025: AlreadyAbsent arm must WARN-log delete_branch failures ──

    /// In-memory captured-log harness mirrored from the existing
    /// `alms-gateway::agents::tests::CapturedLogs` setup used by the
    /// #1029 compensation tests. Writes every `tracing` event the
    /// scoped subscriber receives into an `Arc<Mutex<Vec<u8>>>` so
    /// the test body can grep for structured fields after the
    /// `with_default` scope closes.
    ///
    /// Scoped to a single `with_default(...)` block per test invocation
    /// so parallel `cargo test` jobs don't race on a shared global
    /// subscriber. (`with_default` is known to flake under heavy
    /// parallelism — see #1033 — but the harness is good enough for
    /// a single-call assertion like this one.)
    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
        type Writer = LogWriter;
        fn make_writer(&'a self) -> Self::Writer {
            LogWriter(self.0.clone())
        }
    }

    struct LogWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl CapturedLogs {
        fn captured(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    fn capture_logs<F: FnOnce()>(level: tracing::Level, f: F) -> String {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(level)
            .with_target(true)
            .without_time()
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, f);
        logs.captured()
    }

    /// #1025 regression guard: `remove_worktree` on a missing worktree
    /// directory still tries `git branch -D alms/<name>` as best-effort
    /// cleanup. Pre-fix the failure was silently discarded via
    /// `let _ = delete_branch(...)`, leaving the operator with no
    /// audit trail when the branch was left orphaned. Post-fix the
    /// failure surfaces as a structured WARN at
    /// `target = "alms.worktree"` with the agent name and underlying
    /// error.
    ///
    /// Failure injection: run against a non-git directory. The
    /// `AlreadyAbsent` arm fires (target path doesn't exist), then
    /// `git branch -D` rejects with `fatal: not a git repository`
    /// — stderr doesn't match `delete_branch`'s `not found` / `No such`
    /// success-passthrough, so it returns `Err(WorktreeError::GitFailed(_))`.
    /// Behavior unchanged: the call still returns
    /// `Ok(WorktreeRemove::AlreadyAbsent)`.
    #[test]
    fn remove_worktree_already_absent_warns_on_delete_branch_failure() {
        // Non-git dir: the target worktree path doesn't exist (no
        // `.alms/worktrees/ghost/`) AND `git branch -D` will fail
        // with "not a git repository" inside delete_branch. Both
        // halves of the test setup come for free.
        let tmp = TempDir::new().unwrap();

        let mut outcome: Option<WorktreeResult<WorktreeRemove>> = None;
        let captured = capture_logs(tracing::Level::WARN, || {
            outcome = Some(remove_worktree(tmp.path(), "ghost", false));
        });

        // Behavior unchanged: AlreadyAbsent is still returned, the
        // remove_worktree contract is best-effort on branch cleanup.
        let outcome = outcome
            .expect("test set outcome")
            .expect("remove_worktree must not error");
        assert!(
            matches!(outcome, WorktreeRemove::AlreadyAbsent),
            "remove_worktree on a missing worktree must still return AlreadyAbsent, got {outcome:?}"
        );

        // Audit trail: structured WARN at the alms.worktree target,
        // carrying the agent name and the underlying error message.
        assert!(
            captured.contains("WARN"),
            "expected WARN-level log, got:\n{captured}"
        );
        assert!(
            captured.contains("alms.worktree"),
            "expected target=alms.worktree, got:\n{captured}"
        );
        assert!(
            captured.contains("agent_name=\"ghost\"") || captured.contains("agent_name=ghost"),
            "expected agent_name=ghost field, got:\n{captured}"
        );
        assert!(
            captured.contains("error="),
            "expected error= field, got:\n{captured}"
        );
        assert!(
            captured.contains("AlreadyAbsent"),
            "expected message to identify the AlreadyAbsent path, got:\n{captured}"
        );
    }
}
