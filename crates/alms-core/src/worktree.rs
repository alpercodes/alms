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

/// Create a per-agent worktree at `<project>/.alms/worktrees/<name>/`
/// on branch `alms/<name>`.
///
/// Idempotent in the "already exists" sense: when the worktree
/// directory already exists and points at the right branch, the
/// function returns `Ok(path)` without re-running `git worktree
/// add`. This makes the call safe to retry from agent-create and
/// from a `mode: off → git` flip.
///
/// On a non-git project returns `WorktreeError::NotAGitRepo`. The
/// caller maps this to `400 WORKTREE_REQUIRES_GIT` and refuses to
/// persist the agent record.
pub fn create_worktree(project_root: &Path, agent_name: &str) -> WorktreeResult<PathBuf> {
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
        append_exclude_idempotent(project_root, agent_name)
            .map_err(|e| WorktreeError::Io(format!("append exclude (existing worktree): {e}")))?;
        return Ok(target);
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
/// returns `Ok(())` — matches the agent-delete flow where worktree
/// state may be partially missing if the operator nuked the
/// directory by hand.
pub fn remove_worktree(project_root: &Path, agent_name: &str, force: bool) -> WorktreeResult<()> {
    let target = worktree_path(project_root, agent_name);

    if !target.exists() {
        // Nothing to remove — but also try to clean up the branch
        // ref in case the worktree was nuked manually but the branch
        // still exists. Failures here are non-fatal — the caller's
        // intent is "make this agent gone" and a stray branch ref
        // does not block that.
        let _ = delete_branch(project_root, agent_name, force);
        return Ok(());
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

    Ok(())
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

        let path = create_worktree(tmp.path(), "atlas").expect("create");
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
        // already exists, branch already exists). Nothing to assert
        // beyond "doesn't error".
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        create_worktree(tmp.path(), "atlas").unwrap();
        let result = create_worktree(tmp.path(), "atlas");
        assert!(
            result.is_ok(),
            "second create should be a no-op: {result:?}"
        );
    }

    #[test]
    fn remove_worktree_clean_succeeds() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap();

        remove_worktree(tmp.path(), "atlas", false).unwrap();
        assert!(!path.exists(), "worktree dir should be gone");
    }

    #[test]
    fn remove_worktree_uncommitted_refuses_without_force() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap();

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
        let path = create_worktree(tmp.path(), "atlas").unwrap();
        std::fs::write(path.join("dirty.txt"), "wip").unwrap();

        remove_worktree(tmp.path(), "atlas", true).unwrap();
        assert!(!path.exists(), "force remove should drop the worktree");
    }

    #[test]
    fn remove_worktree_missing_dir_is_noop() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        // Never created — remove should still succeed.
        remove_worktree(tmp.path(), "ghost", false).unwrap();
    }

    #[test]
    fn worktree_has_uncommitted_clean_returns_false() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap();
        assert!(!worktree_has_uncommitted(&path).unwrap());
    }

    #[test]
    fn worktree_has_uncommitted_dirty_returns_true() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(tmp.path());
        let path = create_worktree(tmp.path(), "atlas").unwrap();
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
}
