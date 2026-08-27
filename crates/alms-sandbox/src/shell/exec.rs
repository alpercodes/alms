//! Command execution for the shell tool.
//!
//! Handles spawning processes, capturing output, enforcing timeouts,
//! and detecting the post-execution working directory via a `pwd` marker.

use super::output::truncate_output_bytes;
use super::pathnorm;
use super::security::{command_matches_denylist, is_secret_env_var, platform_critical_env_vars};
use super::spill::{ShellSpillPolicy, write_spill};
use super::types::{
    CwdRejection, CwdRevert, MAX_OUTPUT_BYTES, ShellInput, ShellOutput, ShellState,
};
use crate::{SandboxError, error::SandboxResult};
use alms_core::config::ShellEngine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info, instrument, warn};

/// First ~100 chars of a command, intended for structured log fields.
///
/// Kept short so operator-visible logs never accidentally leak the tail of a
/// long command (which may contain secrets, base64 blobs, etc.).
pub(crate) fn command_excerpt(command: &str) -> String {
    const MAX_LEN: usize = 100;
    if command.len() <= MAX_LEN {
        command.to_string()
    } else {
        // Truncate at a char boundary; appending an ellipsis makes it clear
        // the log is a summary, not the full command.
        let mut end = MAX_LEN;
        while !command.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &command[..end])
    }
}

/// Execute a shell command synchronously (foreground).
///
/// This is the main execution path. It:
/// 1. Validates the command against security checks
/// 2. Builds the process command with cwd, env vars, and timeout
/// 3. Spawns the process and waits for completion
/// 4. Detects the new cwd from the `pwd` marker in output
/// 5. Truncates output if needed
/// 6. Updates the shell state's cwd
///
/// When `spill_policy.is_active()` and the captured stdout or stderr exceeds
/// `MAX_OUTPUT_BYTES`, the full pre-truncation bytes are written to
/// `{run_dir}/shell_{tool_call_id}.log` (issue #756). The returned
/// [`ShellOutput::spill_path`] carries the absolute path so the caller can
/// surface it to the agent.
///
/// When the command ends in a directory outside the sandbox root, the
/// persistent cwd is left where it was and [`ShellOutput::cwd_revert`]
/// records the attempted/kept pair — again for the caller to surface, so the
/// agent learns that its `cd` did not stick (issue #1262).
// The per-execution argument shape intentionally kept flat for readability:
// bundling these into a struct obscures the call path from `ShellTool::execute`
// and `background::submit_background_task` without removing any real
// complexity (every field has a distinct lifetime / ownership story).
#[allow(clippy::too_many_arguments)]
#[instrument(level = "debug", skip(state, sandbox_root, default_env, pwd_marker, spill_policy, tool_call_id), fields(timeout_ms = input.timeout_ms))]
pub(crate) async fn execute_command(
    input: &ShellInput,
    state: &ShellState,
    sandbox_root: Option<&Path>,
    unrestricted: bool,
    default_env: &HashMap<String, String>,
    pwd_marker: &str,
    spill_policy: &ShellSpillPolicy,
    tool_call_id: &str,
) -> SandboxResult<ShellOutput> {
    let command = &input.command;

    if command.trim().is_empty() {
        return Err(SandboxError::InvalidParameters(
            "'command' must not be empty".to_string(),
        ));
    }

    // Security: check for destructive command patterns
    if let Some(pattern) = command_matches_denylist(command) {
        error!(
            tool = "shell",
            reason = "denylist_match",
            pattern = %pattern,
            command_excerpt = %command_excerpt(command),
            "Shell command matched hardcoded denylist"
        );
        return Err(SandboxError::SandboxViolation(format!(
            "Command matches denied pattern '{pattern}'"
        )));
    }

    let cwd = state.cwd.lock().await.clone();

    // Validate cwd against sandbox
    if !unrestricted && let Some(root) = sandbox_root {
        validate_cwd(&cwd, root)?;
    }

    // Build and spawn the command
    let PreparedCommand {
        mut cmd,
        stdin_payload,
    } = build_command(command, &cwd, sandbox_root, unrestricted, pwd_marker)?;

    // Configure environment
    configure_env(&mut cmd, default_env);

    debug!(
        command = %command,
        cwd = %cwd.display(),
        timeout_ms = input.timeout_ms,
        "Spawning shell command"
    );

    let child = cmd
        .spawn()
        .map_err(|e| SandboxError::Io(format!("Failed to spawn command: {e}")))?;

    let result = drive_child_to_completion(child, stdin_payload, input.timeout_ms).await?;

    let exit_code = result.status.code().unwrap_or(-1);

    // Keep stdout/stderr as bytes through cwd extraction and truncation.
    // Lossy UTF-8 decoding happens only at the boundary, so binary-ish output
    // and Windows `\r` are preserved through the pipeline rather than being
    // rewritten to U+FFFD replacement chars mid-flight.
    let raw_stdout = result.stdout;
    let raw_stderr = result.stderr;

    // Extract cwd from pwd marker. The marker and the pwd output are always
    // ASCII so decoding just the stdout bytes as UTF-8 (lossy) for the
    // marker search is safe — replacement chars in user output cannot match
    // the marker pattern.
    //
    // A containment revert is recorded in `cwd_revert` so the tool layer can
    // tell the agent its `cd` did not stick (issue #1262) — the `warn!` below
    // only ever reached the operator's logs.
    let mut cwd_revert: Option<CwdRevert> = None;
    {
        let stdout_for_marker = String::from_utf8_lossy(&raw_stdout);
        if let Some(new_cwd) = extract_cwd_from_output(&stdout_for_marker, pwd_marker) {
            let new_path = PathBuf::from(new_cwd);
            // Validate the new cwd against sandbox root before storing.
            // If the command changed directory outside the sandbox (e.g. `cd /etc`),
            // keep the old cwd to prevent sandbox escape on subsequent calls.
            //
            // #1255: store the NORMALISED path that `validate_cwd` returns,
            // not the raw `pwd` string. The Git-Bash engine reports MSYS form
            // (`/c/dev/ws`) and feeding that straight back to
            // `Command::current_dir` resolves it against the current drive,
            // so even a cwd that passed containment would land the next
            // command somewhere else entirely.
            if !unrestricted && let Some(root) = sandbox_root {
                match check_cwd(&new_path, root) {
                    Ok(resolved) => {
                        let mut cwd_lock = state.cwd.lock().await;
                        *cwd_lock = resolved;
                    }
                    Err(reason) => {
                        warn!(
                            new_cwd = %new_path.display(),
                            sandbox_root = %root.display(),
                            reason = ?reason,
                            "Post-command cwd did not pass containment; keeping previous cwd"
                        );
                        // Read the *live* cwd rather than reusing the
                        // pre-command snapshot: background tasks share one
                        // `ShellState`, so a concurrent command may have
                        // moved it. What the agent needs to be told is where
                        // its next command will actually run.
                        let kept = state.cwd.lock().await.clone();
                        cwd_revert = Some(CwdRevert {
                            attempted: new_path.clone(),
                            kept,
                            reason,
                        });
                    }
                }
            } else {
                // Unsandboxed runs skip containment but still need the MSYS →
                // Windows rewrite for the same `current_dir` reason.
                let mut cwd_lock = state.cwd.lock().await;
                *cwd_lock = pathnorm::resolve_reported_cwd(&new_path);
            }
        }
    }

    // Strip the pwd marker from stdout before truncation so the marker line
    // and cwd line don't count toward the byte budget. Operates on raw bytes.
    let clean_stdout_bytes = strip_pwd_marker_bytes(&raw_stdout, pwd_marker);

    // Spill decision (issue #756): made *before* truncation so we have the
    // full bytes the agent might want to recover. The pre-truncation sizes
    // are what truncate_output_bytes will react to, so checking `> MAX_OUTPUT_BYTES`
    // here matches truncation's own entry condition exactly.
    let stdout_needs_spill = clean_stdout_bytes.len() > MAX_OUTPUT_BYTES;
    let stderr_needs_spill = raw_stderr.len() > MAX_OUTPUT_BYTES;
    let spill_path = if spill_policy.is_active() && (stdout_needs_spill || stderr_needs_spill) {
        // Unwrap is safe: is_active() guarantees run_dir is Some.
        let run_dir = spill_policy
            .run_dir
            .as_deref()
            .expect("is_active() implies run_dir");
        match write_spill(run_dir, tool_call_id, &clean_stdout_bytes, &raw_stderr) {
            Ok(path) => Some(path),
            Err(e) => {
                // Spill failure is non-fatal — the agent still gets the
                // truncated output, just without the recovery file.
                warn!(
                    error = %e,
                    run_dir = %run_dir.display(),
                    tool_call_id = %tool_call_id,
                    "Failed to write shell output spill file; falling back to truncated output only"
                );
                None
            }
        }
    } else {
        None
    };

    // Truncate raw bytes, then decode at the boundary.
    let stdout_truncated = truncate_output_bytes(&clean_stdout_bytes);
    let stderr_truncated = truncate_output_bytes(&raw_stderr);

    // Final lossy decode at the boundary. This is the only place U+FFFD is
    // substituted for invalid UTF-8 — everything upstream stays as bytes.
    let stdout = String::from_utf8_lossy(&stdout_truncated).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_truncated).into_owned();

    Ok(ShellOutput {
        exit_code,
        stdout,
        stderr,
        spill_path,
        cwd_revert,
    })
}

/// Feed `stdin_payload` (builtin engine, #1143) to a spawned child and wait
/// for it to exit, with **one** operator deadline covering the whole
/// interaction.
///
/// The stdin write must sit inside the same `tokio::time::timeout` as
/// `wait_with_output` (PR #1144 review, S1): if the payload exceeds the OS
/// pipe buffer (~64 KiB — agents do emit large heredocs) and the child never
/// drains its stdin (hung child, AV-suspended process, version-skewed binary
/// on disk after an in-place upgrade), `write_all` would otherwise block
/// forever *outside* the timeout that is supposed to be the hard backstop
/// against exactly this kind of pathological child. On deadline expiry the
/// interaction future is dropped, dropping `child` with it — and
/// `kill_on_drop(true)` (set in `build_command`) reaps the process.
async fn drive_child_to_completion(
    mut child: tokio::process::Child,
    stdin_payload: Option<String>,
    timeout_ms: u64,
) -> SandboxResult<std::process::Output> {
    let timeout = std::time::Duration::from_millis(timeout_ms);

    // Take the stdin handle eagerly so a missing-handle bug surfaces as its
    // own error rather than masquerading as a timeout.
    let stdin = match &stdin_payload {
        Some(_) => Some(child.stdin.take().ok_or_else(|| {
            SandboxError::Io("shell-host child has no stdin handle to receive the command".into())
        })?),
        None => None,
    };

    let interaction = async move {
        // Builtin engine (#1143): the wrapped command travels over the
        // child's stdin instead of argv, sidestepping platform quoting and
        // command-line length limits. The shell-host child reads stdin to
        // EOF before evaluating, so closing the handle (shutdown + drop at
        // the end of this block) is what releases it.
        if let (Some(payload), Some(mut stdin)) = (stdin_payload, stdin) {
            use tokio::io::AsyncWriteExt;
            stdin.write_all(payload.as_bytes()).await.map_err(|e| {
                SandboxError::Io(format!("Failed to send command to shell host: {e}"))
            })?;
            stdin
                .shutdown()
                .await
                .map_err(|e| SandboxError::Io(format!("Failed to close shell host stdin: {e}")))?;
            // `stdin` drops here, closing the pipe and releasing the host.
        }

        child
            .wait_with_output()
            .await
            .map_err(|e| SandboxError::Io(format!("Process error: {e}")))
    };

    tokio::time::timeout(timeout, interaction)
        .await
        .map_err(|_| SandboxError::Io(format!("Process timed out after {}s", timeout_ms / 1000)))?
}

/// A command ready to spawn, plus the bytes (if any) that must be written
/// to its stdin and then closed before waiting on it.
///
/// `stdin_payload` is `Some` only for the `builtin` shell engine (#1143),
/// where the wrapped command string travels over stdin to the re-exec'd
/// `alms shell-host` child instead of argv.
struct PreparedCommand {
    cmd: tokio::process::Command,
    stdin_payload: Option<String>,
}

/// Build the tokio Command for execution.
///
/// The command string is wrapped with the pwd marker and handed to a
/// bash-syntax interpreter running in a **child process**. Which
/// interpreter depends on the `[tools].shell_engine` knob (#1143):
///
/// * `system-bash` (default) — `bash -c <wrapped>`. On Unix, `bash` is
///   resolved from `PATH` by the OS as before. On Windows, Git Bash is
///   resolved via [`resolve_shell_binary`]: the operator's
///   `[tools].shell_path` override wins when set; otherwise well-known
///   Git-for-Windows install locations are probed, then candidates derived
///   from the location of `git.exe` on `PATH`. WSL's
///   `C:\Windows\System32\bash.exe` launcher is never an acceptable
///   interpreter — when nothing resolves, the command fails with an
///   actionable error instead of silently executing inside WSL (#1121).
/// * `builtin` — re-exec `std::env::current_exe()` as the hidden
///   `alms shell-host` subcommand, which evaluates the wrapped command via
///   the embedded `brush_core` bash interpreter. Zero `PATH` resolution.
///   The wrapped command is piped over stdin (see [`PreparedCommand`]).
///
/// We use bash syntax (not `cmd /c` / PowerShell) in both engines because
/// the pwd marker, exit code capture, the destructive-command classifier,
/// and operator `shell_permissions` regexes all assume POSIX semantics.
///
/// On Linux 5.13+ with a sandbox root configured, Landlock filesystem
/// restrictions are applied via `pre_exec` so the child process can only
/// access files within the sandbox root — identically for both engines.
fn build_command(
    command: &str,
    cwd: &Path,
    sandbox_root: Option<&Path>,
    unrestricted: bool,
    pwd_marker: &str,
) -> SandboxResult<PreparedCommand> {
    build_command_for_engine(
        shell_engine(),
        command,
        cwd,
        sandbox_root,
        unrestricted,
        pwd_marker,
    )
}

/// Engine-parameterized core of [`build_command`].
///
/// Split out so unit tests can exercise both engines without mutating the
/// process-global [`SHELL_ENGINE`] state.
fn build_command_for_engine(
    engine: ShellEngine,
    command: &str,
    cwd: &Path,
    sandbox_root: Option<&Path>,
    unrestricted: bool,
    pwd_marker: &str,
) -> SandboxResult<PreparedCommand> {
    let wrapped = wrap_command_with_pwd_marker(command, pwd_marker);

    // Extra paths the Landlock sandbox must keep readable+executable for
    // the child to start at all. Empty for system-bash (bash lives under
    // the always-granted system paths); for builtin it carries the ALMS
    // binary itself, which may live anywhere (e.g. ~/bin, target/debug).
    let mut extra_landlock_exec_paths: Vec<PathBuf> = Vec::new();

    let mut prepared = match engine {
        ShellEngine::SystemBash => {
            let bash_bin = resolve_shell_binary().map_err(SandboxError::Io)?;

            let mut cmd = tokio::process::Command::new(bash_bin);
            cmd.arg("-c");
            cmd.arg(&wrapped);
            PreparedCommand {
                cmd,
                stdin_payload: None,
            }
        }
        ShellEngine::Builtin => {
            // Re-exec our own binary — deterministic by construction, no
            // PATH or install-location resolution anywhere (#1143).
            let exe = std::env::current_exe().map_err(|e| {
                SandboxError::Io(format!(
                    "shell_engine = \"builtin\": cannot determine the current \
                     executable to re-exec as `alms shell-host`: {e}"
                ))
            })?;
            extra_landlock_exec_paths.push(exe.clone());

            let mut cmd = tokio::process::Command::new(exe);
            cmd.arg("shell-host");
            // The command string travels over stdin, not argv (quoting and
            // command-line length limits). stdout/stderr stay piped below.
            cmd.stdin(std::process::Stdio::piped());
            PreparedCommand {
                cmd,
                stdin_payload: Some(wrapped),
            }
        }
    };

    let cmd = &mut prepared.cmd;
    cmd.current_dir(cwd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    cmd.kill_on_drop(true);

    // Apply Landlock filesystem sandboxing on Linux when a sandbox root is configured
    #[cfg(target_os = "linux")]
    if !unrestricted && let Some(root) = sandbox_root {
        apply_landlock_sandbox(cmd, root, &extra_landlock_exec_paths);
    }

    // Suppress unused variable warnings on non-Linux platforms
    #[cfg(not(target_os = "linux"))]
    {
        let _ = sandbox_root;
        let _ = unrestricted;
        let _ = &extra_landlock_exec_paths;
    }

    Ok(prepared)
}

// ---------------------------------------------------------------------------
// Shell binary resolution (#1121)
// ---------------------------------------------------------------------------

/// Operator-supplied `[tools].shell_path` override, installed once at boot
/// via [`init_shell_resolution`]. `None` inside the `OnceLock` means the
/// knob was explicitly unset; an unset `OnceLock` (library users that never
/// call init) behaves identically.
static SHELL_PATH_OVERRIDE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

fn shell_path_override() -> Option<&'static Path> {
    SHELL_PATH_OVERRIDE.get().and_then(|o| o.as_deref())
}

/// Operator-selected `[tools].shell_engine` (#1143), installed once at boot
/// via [`init_shell_resolution`]. Library users that never call init get
/// the default (`SystemBash`) — identical to pre-#1143 behavior.
static SHELL_ENGINE: std::sync::OnceLock<ShellEngine> = std::sync::OnceLock::new();

fn shell_engine() -> ShellEngine {
    SHELL_ENGINE.get().copied().unwrap_or_default()
}

/// Install the operator's `[tools].shell_path` override (if any) plus the
/// `[tools].shell_engine` selection (#1143), and log the resolved shell
/// interpreter once at `target = "alms.security"`.
///
/// Called by the gateway at boot so operators learn the active shell before
/// the first agent run — not from garbled tool output. Idempotent: the first
/// call wins; a later call with a *different* value logs a WARN and keeps
/// the original.
pub fn init_shell_resolution(shell_path: Option<PathBuf>, engine: ShellEngine) {
    if SHELL_PATH_OVERRIDE.set(shell_path.clone()).is_err()
        && shell_path_override() != shell_path.as_deref()
    {
        warn!(
            target: "alms.security",
            "init_shell_resolution called again with a different [tools].shell_path; \
             keeping the first value: {:?}",
            shell_path_override()
        );
    }
    if SHELL_ENGINE.set(engine).is_err() && shell_engine() != engine {
        warn!(
            target: "alms.security",
            "init_shell_resolution called again with a different [tools].shell_engine; \
             keeping the first value: {:?}",
            shell_engine()
        );
    }
    match shell_engine() {
        ShellEngine::Builtin => {
            if shell_path_override().is_some() {
                warn!(
                    target: "alms.security",
                    "[tools].shell_path is set but [tools].shell_engine = \"builtin\" \
                     never resolves an external shell — the override is ignored"
                );
            }
            match std::env::current_exe() {
                Ok(exe) => info!(
                    target: "alms.security",
                    host_binary = %exe.display(),
                    "shell tool: builtin engine — commands run via re-exec'd \
                     `alms shell-host` (embedded brush bash interpreter)"
                ),
                Err(e) => warn!(
                    target: "alms.security",
                    "shell tool: builtin engine selected but current_exe() failed — \
                     shell commands will fail: {e}"
                ),
            }
        }
        ShellEngine::SystemBash => match resolve_shell_binary() {
            Ok(path) => info!(
                target: "alms.security",
                shell_path = %path.display(),
                override_set = shell_path_override().is_some(),
                "shell tool: commands will execute via this interpreter"
            ),
            Err(msg) => warn!(
                target: "alms.security",
                "shell tool: no usable shell interpreter found — shell commands will fail: {msg}"
            ),
        },
    }
}

/// Resolve the shell binary `build_command` will spawn.
///
/// Order:
/// 1. Operator `[tools].shell_path` override — wins unconditionally when
///    set. A missing file or a System32/Sysnative path is a hard error,
///    never a silent fall-through to discovery.
/// 2. (Windows) Git Bash discovery, memoized per process — well-known
///    install locations first, then candidates derived from the location
///    of `git.exe` on `PATH` (see [`bash_candidates`]).
/// 3. (Unix) `bash`, resolved from `PATH` by the OS — unchanged.
///
/// On Windows there is deliberately **no** bare-`"bash"` fallback: with Git
/// Bash's `bin` directory off `PATH` (the common case — only Git's `cmd`
/// dir is on `PATH`), bare `bash` resolves to
/// `C:\Windows\System32\bash.exe`, the WSL launcher, and commands silently
/// execute in the wrong operating environment (#1121).
fn resolve_shell_binary() -> Result<PathBuf, String> {
    let override_path = shell_path_override();
    if override_path.is_some() {
        // Not memoized: validation is a single stat per spawn, and staying
        // live means an operator who fixes the file the knob points at does
        // not need a daemon restart.
        return resolve_shell_path(override_path, &[], |p: &Path| p.is_file());
    }
    #[cfg(windows)]
    {
        resolve_bash_path().cloned()
    }
    #[cfg(not(windows))]
    {
        Ok(PathBuf::from("bash"))
    }
}

/// Resolve the Git-Bash executable once per process and cache the result.
///
/// Discovery performs a bounded number of `is_file()` stats plus one walk
/// of the `PATH` entries looking for `git.exe`. Since the answer is stable
/// for the lifetime of the process, the resolution is memoized in a
/// `LazyLock` so `build_command` pays the cost only on the first
/// invocation. (Installing Git for Windows after daemon start therefore
/// requires a daemon restart to be picked up.)
#[cfg(windows)]
fn resolve_bash_path() -> Result<&'static PathBuf, String> {
    static BASH_PATH: std::sync::LazyLock<Result<PathBuf, String>> =
        std::sync::LazyLock::new(discover_bash_path);
    BASH_PATH.as_ref().map_err(Clone::clone)
}

#[cfg(windows)]
fn discover_bash_path() -> Result<PathBuf, String> {
    let candidates = bash_candidates(
        &windows_static_bash_candidates(),
        find_git_exe_dir_on_path().as_deref(),
    );
    resolve_shell_path(None, &candidates, |p: &Path| p.is_file())
}

/// Pure resolution core — unit-testable without touching the live
/// filesystem or environment.
///
/// * `override_path` — operator `[tools].shell_path`. When `Some`, it wins
///   unconditionally: a System32/Sysnative path or a non-file is a hard
///   error, never a fall-through to `candidates`.
/// * `candidates` — ordered discovery candidates (see [`bash_candidates`]).
///   The first entry that is a file and is not under System32/Sysnative
///   wins.
/// * `is_file` — injected filesystem probe. The real callers pass
///   `Path::is_file` (not `exists()` — a *directory* named `bash.exe` must
///   not match).
fn resolve_shell_path(
    override_path: Option<&Path>,
    candidates: &[PathBuf],
    is_file: impl Fn(&Path) -> bool,
) -> Result<PathBuf, String> {
    if let Some(p) = override_path {
        if is_under_windows_system32(p) {
            return Err(format!(
                "[tools].shell_path points at '{}', which is under System32/Sysnative — \
                 that bash.exe is the WSL launcher, not a shell. Point shell_path at a \
                 real shell executable (e.g. Git Bash's <GitRoot>\\bin\\bash.exe).",
                p.display()
            ));
        }
        if !is_file(p) {
            return Err(format!(
                "[tools].shell_path points at '{}', which does not exist or is not a \
                 file. Fix the path in alms.toml, or remove the knob to use built-in \
                 discovery.",
                p.display()
            ));
        }
        return Ok(p.to_path_buf());
    }

    for candidate in candidates {
        // Belt-and-suspenders: `bash_candidates` already filters these, but
        // no candidate source — present or future — may route to the WSL
        // launcher.
        if is_under_windows_system32(candidate) {
            continue;
        }
        if is_file(candidate) {
            return Ok(candidate.clone());
        }
    }

    Err(
        "Git Bash not found on this Windows host. The shell tool refuses to fall back \
         to bare 'bash' because PATH resolution would silently spawn the WSL launcher \
         (C:\\Windows\\System32\\bash.exe). Install Git for Windows, or set \
         [tools].shell_path in alms.toml to the absolute path of a bash executable."
            .to_string(),
    )
}

/// True when `path` lives under a Windows `System32` or `Sysnative`
/// directory — home of the WSL launcher `bash.exe`, which is never an
/// acceptable shell interpreter (#1121).
///
/// Compared case-insensitively on normalized separators so
/// `C:/WINDOWS/system32/bash.exe` and `\\?\C:\Windows\Sysnative\bash.exe`
/// are both caught. String-based (rather than `Path::components`) so the
/// check behaves identically — and is unit-testable — on non-Windows hosts,
/// where backslashes are not path separators.
fn is_under_windows_system32(path: &Path) -> bool {
    let normalized = path
        .to_string_lossy()
        .to_ascii_lowercase()
        .replace('/', "\\");
    normalized.contains("\\system32\\")
        || normalized.contains("\\sysnative\\")
        || normalized.ends_with("\\system32")
        || normalized.ends_with("\\sysnative")
}

/// Ordered Git-Bash candidate list: static well-known install locations
/// first, then candidates derived from the directory containing `git.exe`
/// (`<GitRoot>\cmd\git.exe` → `<GitRoot>\bin\bash.exe` and
/// `<GitRoot>\usr\bin\bash.exe`).
///
/// `git.exe` on `PATH` is a far more reliable anchor than hardcoded install
/// roots — it survives portable, non-`C:`-drive, and custom-prefix installs
/// — and by construction can never point at the WSL launcher.
/// System32/Sysnative entries are filtered out here, belt-and-suspenders
/// with [`resolve_shell_path`].
#[cfg_attr(not(windows), allow(dead_code))]
fn bash_candidates(static_candidates: &[PathBuf], git_exe_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = static_candidates
        .iter()
        .filter(|c| !is_under_windows_system32(c))
        .cloned()
        .collect();

    if let Some(dir) = git_exe_dir
        && let Some(git_root) = dir.parent()
    {
        for derived in [
            git_root.join("bin").join("bash.exe"),
            git_root.join("usr").join("bin").join("bash.exe"),
        ] {
            if !is_under_windows_system32(&derived) && !out.contains(&derived) {
                out.push(derived);
            }
        }
    }

    out
}

/// The pre-#1121 static probe list, expressed as candidate paths: standard
/// Git-for-Windows roots (literal `C:\` plus the `ProgramFiles`-family env
/// vars, which differ on non-`C:` system drives) with `bin\bash.exe` and
/// `usr\bin\bash.exe` under each.
#[cfg(windows)]
fn windows_static_bash_candidates() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = vec![
        PathBuf::from("C:\\Program Files\\Git"),
        PathBuf::from("C:\\Program Files (x86)\\Git"),
    ];
    for var in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Ok(v) = std::env::var(var) {
            roots.push(Path::new(&v).join("Git"));
        }
    }
    if let Ok(local) = std::env::var("LocalAppData") {
        roots.push(Path::new(&local).join("Programs").join("Git"));
    }
    if let Ok(up) = std::env::var("USERPROFILE") {
        roots.push(
            Path::new(&up)
                .join("AppData")
                .join("Local")
                .join("Programs")
                .join("Git"),
        );
    }

    let mut out: Vec<PathBuf> = Vec::with_capacity(roots.len() * 2);
    for root in roots {
        for candidate in [
            root.join("bin").join("bash.exe"),
            root.join("usr").join("bin").join("bash.exe"),
        ] {
            if !out.contains(&candidate) {
                out.push(candidate);
            }
        }
    }
    out
}

/// Locate the directory containing `git.exe` by walking the process `PATH`
/// (a proven trick — see #1121). System32/Sysnative entries are
/// skipped: we are anchoring on Git for Windows, and the WSL launcher's
/// home can never be it.
#[cfg(windows)]
fn find_git_exe_dir_on_path() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() || is_under_windows_system32(&dir) {
            continue;
        }
        if dir.join("git.exe").is_file() {
            return Some(dir);
        }
    }
    None
}

/// Apply Landlock filesystem restrictions to a command via `pre_exec`.
///
/// This restricts the child process (and all its descendants) to only
/// access files under `sandbox_root`. The restriction is applied using
/// Linux's Landlock LSM (available since Linux 5.13).
///
/// If Landlock is not supported by the running kernel, a warning is logged
/// and execution continues without filesystem restrictions.
///
/// `extra_exec_paths` are granted the same read+execute access as the
/// standard system paths. The builtin shell engine (#1143) passes the ALMS
/// binary itself here: the re-exec'd `alms shell-host` child must remain
/// executable under the ruleset even when the binary lives outside
/// `/usr`/`/bin`/`/lib` (e.g. `~/bin/alms`, `target/debug/alms`).
#[cfg(target_os = "linux")]
fn apply_landlock_sandbox(
    cmd: &mut tokio::process::Command,
    sandbox_root: &Path,
    extra_exec_paths: &[PathBuf],
) {
    // Canonicalize the sandbox root so the Landlock rules match real paths.
    // If canonicalization fails (dir doesn't exist yet), fall back to the
    // provided path — Landlock will simply deny all access, which is safer
    // than allowing everything.
    let canonical_root =
        std::fs::canonicalize(sandbox_root).unwrap_or_else(|_| sandbox_root.to_path_buf());

    // We also need to allow access to standard system paths so bash and
    // basic utilities can execute. These are read-only.
    //
    // NOTE: /etc is narrowed to specific entries that bash/coreutils need.
    // /tmp is excluded — commands that need temp space should use the sandbox root.
    //
    // /etc/passwd is intentionally NOT granted (issue #743 / #734 item 2):
    // exposing it enables user enumeration on shared hosts. The trade-off is
    // that `~user` tilde expansion to *other* users' home directories no
    // longer works, and `ls -l`/`whoami` may print numeric UIDs instead of
    // names. `~/path` (current user) still works because bash uses `$HOME`
    // for that, which doesn't read /etc/passwd. See `docs/security-model.md`
    // for the full rationale.
    let mut system_read_paths: Vec<PathBuf> = vec![
        PathBuf::from("/usr"),
        PathBuf::from("/bin"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        // Specific /etc entries needed by the dynamic linker and coreutils:
        PathBuf::from("/etc/ld.so.cache"),
        PathBuf::from("/etc/ld.so.conf"),
        PathBuf::from("/etc/ld.so.conf.d"),
        PathBuf::from("/etc/alternatives"),
        PathBuf::from("/etc/nsswitch.conf"),
        PathBuf::from("/etc/resolv.conf"),
        PathBuf::from("/etc/localtime"),
        PathBuf::from("/dev/null"),
        PathBuf::from("/dev/urandom"),
        PathBuf::from("/dev/zero"),
        PathBuf::from("/proc/self"),
    ];
    system_read_paths.extend_from_slice(extra_exec_paths);

    // Clone what we need into the closure (pre_exec runs after fork, before exec)
    let root_for_closure = canonical_root.clone();
    let sys_paths = system_read_paths;

    // SAFETY: pre_exec runs in the child process after fork() but before
    // exec(). The Landlock syscalls themselves are async-signal-safe, but
    // we use eprintln! for diagnostics on error paths, which is technically
    // not async-signal-safe (it may allocate). In practice this is reliable
    // after fork() on Linux and only executes on error paths. The trade-off
    // is accepted for debuggability.
    unsafe {
        cmd.pre_exec(move || {
            use landlock::{
                ABI, Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr,
                RulesetCreatedAttr, RulesetStatus,
            };

            // Request V5 features; the landlock crate will degrade to the
            // highest ABI the running kernel supports (best-effort).
            let abi = ABI::V5;

            let read_access = AccessFs::ReadFile | AccessFs::ReadDir | AccessFs::Execute;

            // NOTE: AccessFs::Refer is intentionally not granted for any path.
            // This means rename()/link() across differently-ruled directories
            // will be denied. rename() within the sandbox root works because
            // source and target share the same ruleset entry. This prevents
            // using rename to move files from system paths into the sandbox.
            let write_access = read_access
                | AccessFs::WriteFile
                | AccessFs::MakeReg
                | AccessFs::MakeDir
                | AccessFs::MakeSym
                | AccessFs::RemoveFile
                | AccessFs::RemoveDir
                | AccessFs::Truncate;

            let ruleset = match Ruleset::default().handle_access(AccessFs::from_all(abi)) {
                Ok(r) => r,
                Err(e) => {
                    // handle_access() failure means Landlock is not supported
                    // by this kernel — gracefully degrade to unsandboxed execution.
                    eprintln!("[alms] Landlock not supported by kernel, running unsandboxed: {e}");
                    return Ok(());
                }
            };

            // From this point on, the kernel supports Landlock. Any failure
            // to create or enforce the ruleset is a hard error — we must NOT
            // run the command unsandboxed when sandboxing was requested and
            // the kernel confirmed it can apply rules.

            let ruleset = match ruleset.create() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[alms] Landlock ruleset create failed: {e}");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("Landlock ruleset creation failed: {e}"),
                    ));
                }
            };

            // Allow full read+write access to the sandbox root
            let fd = match PathFd::new(&root_for_closure) {
                Ok(fd) => fd,
                Err(e) => {
                    eprintln!(
                        "[alms] Landlock: cannot open sandbox root '{}': {e}",
                        root_for_closure.display()
                    );
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "Landlock: cannot open sandbox root '{}': {e}",
                            root_for_closure.display()
                        ),
                    ));
                }
            };
            let rule = PathBeneath::new(fd, write_access);
            let ruleset = match ruleset.add_rule(rule) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[alms] Landlock: failed to add sandbox root rule: {e}");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("Landlock: failed to add sandbox root rule: {e}"),
                    ));
                }
            };

            // Allow read+execute access to system paths (missing paths are
            // skipped since not all distros have the same layout, but if a
            // present path fails to be added, that is a hard error — bash
            // won't work under the sandbox without /usr, /lib, etc.)
            let mut ruleset = ruleset;
            for sys_path in &sys_paths {
                if sys_path.exists()
                    && let Ok(fd) = PathFd::new(sys_path)
                {
                    let rule = PathBeneath::new(fd, read_access);
                    ruleset = match ruleset.add_rule(rule) {
                        Ok(r) => r,
                        Err(e) => {
                            eprintln!(
                                "[alms] Landlock: failed to add system path rule for {}: {e}",
                                sys_path.display()
                            );
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                format!("Landlock: failed to add system path rule: {e}"),
                            ));
                        }
                    };
                }
            }

            match ruleset.restrict_self() {
                Ok(status) => {
                    if matches!(status.ruleset, RulesetStatus::NotEnforced) {
                        eprintln!("[alms] Landlock: ruleset was not enforced by kernel");
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::PermissionDenied,
                            "Landlock ruleset was not enforced by kernel",
                        ));
                    }
                }
                Err(e) => {
                    eprintln!("[alms] Landlock restrict_self failed: {e}");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("Landlock restrict_self failed: {e}"),
                    ));
                }
            }

            Ok(())
        });
    }

    debug!(
        sandbox_root = %canonical_root.display(),
        "Landlock filesystem sandbox configured for child process"
    );
}

/// Wrap a command string with a pwd marker so we can detect cwd changes.
///
/// The marker is appended after the command via `; echo <MARKER>; pwd`.
/// This way, even if the command fails, we still get the working directory.
///
/// The marker is supplied by the caller as a per-`ShellTool` random nonce
/// (see `ShellTool::new`). Using a per-instance nonce instead of a fixed
/// constant prevents a user script that intentionally or accidentally prints
/// the marker string from corrupting cwd tracking on a different agent
/// instance — each ShellTool only matches its own marker.
fn wrap_command_with_pwd_marker(command: &str, pwd_marker: &str) -> String {
    // Use a subshell to capture the command's output, then echo the marker
    // and the current working directory.
    format!("{command}; __alms_ec=$?; echo; echo '{pwd_marker}'; pwd; exit $__alms_ec")
}

/// Extract the cwd from command output by looking for the **last** PWD marker.
///
/// We match the last occurrence because command output could contain the
/// literal marker string. The real marker is always appended last by
/// `wrap_command_with_pwd_marker`.
///
/// Returns `Some(path_str)` if found, `None` if the marker is not present.
fn extract_cwd_from_output<'a>(stdout: &'a str, pwd_marker: &str) -> Option<&'a str> {
    let lines: Vec<&str> = stdout.lines().collect();
    // Search backwards for the last marker line
    let marker_idx = lines.iter().rposition(|line| line.trim() == pwd_marker)?;
    // The cwd is the next non-empty line after the marker
    for line in &lines[marker_idx + 1..] {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}

/// Byte-level variant of strip_pwd_marker used by the execution path.
///
/// Operates on the raw process-output bytes so lossy UTF-8 decoding is
/// deferred to the final boundary. The marker is ASCII so byte-level line
/// scanning is sufficient. Returns the bytes before the **last** marker
/// line; earlier occurrences (which could only originate from user output)
/// are preserved as user data, matching the semantics of the previous
/// string-level helper.
fn strip_pwd_marker_bytes(stdout: &[u8], pwd_marker: &str) -> Vec<u8> {
    let marker_bytes = pwd_marker.as_bytes();

    // Collect the byte ranges of each line. `end` excludes the trailing
    // '\n' (and the preceding '\r' if it forms a CRLF pair).
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    for (i, &b) in stdout.iter().enumerate() {
        if b == b'\n' {
            let mut end = i;
            if end > start && stdout[end - 1] == b'\r' {
                end -= 1;
            }
            lines.push((start, end));
            start = i + 1;
        }
    }
    if start < stdout.len() {
        lines.push((start, stdout.len()));
    }

    // Trim ASCII whitespace from each line before comparing to the marker.
    let is_marker = |range: &(usize, usize)| -> bool {
        let (s, e) = *range;
        let trimmed = trim_ascii_whitespace(&stdout[s..e]);
        trimmed == marker_bytes
    };

    let Some(marker_idx) = lines.iter().rposition(is_marker) else {
        // No marker — return the input bytes as-is.
        return stdout.to_vec();
    };

    // Take all bytes up to the beginning of the marker line.
    let mut end_byte = lines[marker_idx].0;

    // Strip exactly the one separator newline emitted by `wrap_command_with_pwd_marker`'s
    // bare `echo;` between the user's command and the marker. That separator is a single
    // `\n` (or `\r\n` if the upstream stream happened to be CRLF) — nothing more.
    //
    // CRITICAL: do NOT walk back over arbitrary trailing newlines. The user's command may
    // legitimately end in `\n` (e.g. `printf '...'; echo` produces a trailing `\n` that
    // belongs to the user's output), and that byte must survive into the captured stdout
    // so byte-for-byte spill round-trips work. See #954.
    //
    // Known minor lossy edge case: if the user's output ends in a raw `\r` with no
    // following `\n` (i.e. wrapper bytes are `...a\r\n{MARKER}...`), the `\r` here is
    // indistinguishable from the leading half of a CRLF separator and gets stripped
    // alongside the `\n`. This is identical to the pre-#954 behavior and is realistic
    // only for very obscure binary-ish output; fixing it would require the wrapper to
    // emit a non-newline sentinel before the separator. Not worth the complexity today.
    if end_byte > 0 && stdout[end_byte - 1] == b'\n' {
        end_byte -= 1;
        if end_byte > 0 && stdout[end_byte - 1] == b'\r' {
            end_byte -= 1;
        }
    }

    stdout[..end_byte].to_vec()
}

/// Trim ASCII whitespace from both ends of a byte slice without allocating.
fn trim_ascii_whitespace(s: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = s.len();
    while start < end && (s[start] == b' ' || s[start] == b'\t') {
        start += 1;
    }
    while end > start && (s[end - 1] == b' ' || s[end - 1] == b'\t') {
        end -= 1;
    }
    &s[start..end]
}

/// Decide whether a cwd may be adopted, and on rejection say *why*.
///
/// Returns the **normalised** cwd on success so the caller can store a path
/// this process can actually hand to `Command::current_dir`. That return
/// value is load-bearing on Windows: the Git-Bash engine reports its cwd in
/// MSYS form (`/c/dev/ws`), which `current_dir` would otherwise resolve
/// against the current drive as `C:\c\dev\ws`.
///
/// #1255: both sides are normalised via [`pathnorm`] before comparison
/// rather than string-matched. The pre-fix version compared whatever form
/// each side happened to be in — an MSYS `pwd` against a `\\?\`-prefixed
/// canonicalised root — so the sandbox decided its own root was outside
/// itself and reverted every legitimate `cd`.
///
/// #1262: the rejection carries a [`CwdRejection`] because the accept/reject
/// bit alone does not identify the cause. `pathnorm::normalise` falls back to
/// its raw input when canonicalisation fails, so an unresolvable path lands
/// in exactly the same reject arm as a real escape. Deriving the reason here,
/// from the same two `Normalised` values the verdict was computed from, is
/// what keeps it in step with the verdict — a separate classifier run later
/// could disagree with the decision it purports to explain.
///
/// **Behaviour is unchanged**: the accept/reject decision is still
/// `is_within` over the same two normalised paths, and every rejection still
/// fails closed. Only the explanation is new.
fn check_cwd(cwd: &Path, sandbox_root: &Path) -> Result<PathBuf, CwdRejection> {
    let root = pathnorm::normalise(sandbox_root);
    let candidate = pathnorm::resolve_reported(cwd);

    if pathnorm::is_within(&root.path, &candidate.path) {
        return Ok(candidate.path);
    }

    // "Outside the root" is a claim about where the path *is*, and the
    // comparison above only supports it when both sides were real resolved
    // paths. If either fell back to its raw spelling, all that was
    // established is that containment could not be confirmed.
    Err(if root.resolved && candidate.resolved {
        CwdRejection::OutsideRoot
    } else {
        CwdRejection::NotVerifiable
    })
}

/// Validate that a cwd is within the sandbox root, as a [`SandboxError`].
///
/// Thin wrapper over [`check_cwd`] for the pre-command check, which has no
/// use for the structured reason beyond wording the message accurately.
fn validate_cwd(cwd: &Path, sandbox_root: &Path) -> SandboxResult<PathBuf> {
    check_cwd(cwd, sandbox_root).map_err(|reason| {
        let verdict = match reason {
            CwdRejection::OutsideRoot => "is outside",
            CwdRejection::NotVerifiable => "could not be confirmed inside",
        };
        SandboxError::SandboxViolation(format!(
            "Working directory '{}' {verdict} sandbox root '{}'",
            cwd.display(),
            sandbox_root.display()
        ))
    })
}

/// Configure the environment for a spawned process.
///
/// 1. Clears all inherited env vars (prevents secret leakage)
/// 2. Re-injects platform-critical vars (PATH, SystemRoot, etc.)
/// 3. Injects default_env vars (ALMS_DATA_DIR, etc.)
/// 4. Filters out secret env vars from all sources
fn configure_env(cmd: &mut tokio::process::Command, default_env: &HashMap<String, String>) {
    cmd.env_clear();

    // Re-inject platform-critical env vars
    for key in platform_critical_env_vars() {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }

    // Inject default env vars (filtering secrets)
    for (k, v) in default_env {
        if is_secret_env_var(k) {
            warn!(env_var = %k, "Blocked secret env var from default_env injection");
            continue;
        }
        cmd.env(k, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canned marker used by the unit tests. Production code uses a
    /// per-instance random nonce generated in `ShellTool::new`.
    const TEST_MARKER: &str = "__ALMS_PWD_TEST_MARKER__";

    #[test]
    fn test_wrap_command_with_pwd_marker() {
        let wrapped = wrap_command_with_pwd_marker("ls -la", TEST_MARKER);
        assert!(wrapped.contains("ls -la"));
        assert!(wrapped.contains(TEST_MARKER));
        assert!(wrapped.contains("pwd"));
    }

    #[test]
    fn test_extract_cwd_from_output() {
        let output = format!("file1.txt\nfile2.txt\n\n{TEST_MARKER}\n/home/user/project\n");
        assert_eq!(
            extract_cwd_from_output(&output, TEST_MARKER),
            Some("/home/user/project")
        );
    }

    #[test]
    fn test_extract_cwd_no_marker() {
        let output = "file1.txt\nfile2.txt\n";
        assert_eq!(extract_cwd_from_output(output, TEST_MARKER), None);
    }

    #[test]
    fn test_strip_pwd_marker_bytes() {
        // Layout: <user-output>\n<separator-from-bare-echo>\n<marker>\n<pwd>\n
        // Here the user's output is "file1.txt\nfile2.txt\n" — the trailing \n
        // belongs to the user and must survive. Only the single separator \n
        // (and the marker line and pwd line) get stripped.
        let output = format!("file1.txt\nfile2.txt\n\n{TEST_MARKER}\n/home/user\n");
        let cleaned = strip_pwd_marker_bytes(output.as_bytes(), TEST_MARKER);
        let cleaned_str = String::from_utf8(cleaned).unwrap();
        assert_eq!(cleaned_str, "file1.txt\nfile2.txt\n");
        assert!(!cleaned_str.contains(TEST_MARKER));
        assert!(!cleaned_str.contains("/home/user"));
    }

    #[test]
    fn test_strip_pwd_marker_bytes_minimal_wrapper_shape() {
        // Tim's suggested contract test (PR #955): pure-byte assertion on the
        // canonical `wrap_command_with_pwd_marker` output shape, independent of
        // bash availability. Locks #954 across every platform.
        //
        // Layout: <user>\n<separator>\n<marker>\n<pwd>\n
        // Input : "aaa\n\n{MARKER}\n/home\n"
        // Output: "aaa\n"  (user's trailing \n survives; the bare-`echo` separator
        //                   and the marker/pwd lines are stripped — nothing more)
        let input = format!("aaa\n\n{TEST_MARKER}\n/home\n");
        let cleaned = strip_pwd_marker_bytes(input.as_bytes(), TEST_MARKER);
        assert_eq!(cleaned, b"aaa\n");
    }

    #[test]
    fn test_strip_pwd_marker_bytes_no_marker() {
        let output = b"line1\nline2\n";
        let cleaned = strip_pwd_marker_bytes(output, TEST_MARKER);
        // No marker, bytes are returned unchanged.
        assert_eq!(cleaned, output.to_vec());
    }

    #[test]
    fn test_strip_pwd_marker_bytes_preserves_crlf() {
        // Windows shells emit \r\n; we must NOT strip \r from user output.
        let output = b"line1\r\nline2\r\n\r\n__ALMS_PWD_TEST_MARKER__\r\n/home/user\r\n";
        let cleaned = strip_pwd_marker_bytes(output, TEST_MARKER);
        let cleaned_str = String::from_utf8(cleaned).unwrap();
        assert!(
            cleaned_str.contains("line1\r\n"),
            "CRLF on user output must survive: {cleaned_str:?}"
        );
        assert!(cleaned_str.contains("line2"));
        assert!(!cleaned_str.contains("__ALMS_PWD_TEST_MARKER__"));
        assert!(!cleaned_str.contains("/home/user"));
    }

    #[test]
    fn test_strip_pwd_marker_bytes_preserves_invalid_utf8() {
        // Binary-ish stdout must pass through the byte truncation step
        // unchanged until the lossy-decode boundary at the end.
        let mut output = Vec::new();
        output.extend_from_slice(b"before\n");
        output.push(0xFF); // invalid UTF-8 byte
        output.extend_from_slice(b"\n\n");
        output.extend_from_slice(TEST_MARKER.as_bytes());
        output.extend_from_slice(b"\n/home/user\n");

        let cleaned = strip_pwd_marker_bytes(&output, TEST_MARKER);
        assert!(cleaned.contains(&0xFF), "raw bytes must be preserved");
        assert!(
            !cleaned
                .windows(TEST_MARKER.len())
                .any(|w| w == TEST_MARKER.as_bytes())
        );
    }

    #[test]
    fn test_extract_cwd_matches_last_marker() {
        // If command output contains the marker string, we must match the LAST one
        let output = format!(
            "echo {TEST_MARKER}\n/fake/path\nreal output\n\n{TEST_MARKER}\n/home/user/real\n"
        );
        assert_eq!(
            extract_cwd_from_output(&output, TEST_MARKER),
            Some("/home/user/real")
        );
    }

    #[test]
    fn test_strip_pwd_marker_bytes_matches_last_marker() {
        // Only the last marker should be stripped; earlier occurrences are user data
        let output = format!(
            "echo {TEST_MARKER}\n/fake/path\nreal output\n\n{TEST_MARKER}\n/home/user/real\n"
        );
        let cleaned = strip_pwd_marker_bytes(output.as_bytes(), TEST_MARKER);
        let cleaned_str = String::from_utf8(cleaned).unwrap();
        assert!(
            cleaned_str.contains(TEST_MARKER),
            "first marker should be preserved as user data"
        );
        assert!(cleaned_str.contains("/fake/path"));
        assert!(cleaned_str.contains("real output"));
        assert!(
            !cleaned_str.contains("/home/user/real"),
            "real cwd line should be stripped"
        );
    }

    #[test]
    fn test_command_excerpt_short_returns_full() {
        assert_eq!(command_excerpt("ls -la"), "ls -la");
    }

    #[test]
    fn test_command_excerpt_long_is_truncated() {
        let long = "x".repeat(500);
        let excerpt = command_excerpt(&long);
        // For ASCII input we should be exactly 100 + ellipsis bytes.
        assert!(excerpt.len() < long.len());
        assert!(excerpt.ends_with('…'));
        assert_eq!(&excerpt[..100], &long[..100]);
    }

    #[test]
    fn test_build_command_wraps_with_bash() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path();
        let prepared = build_command("echo hello", cwd, None, true, TEST_MARKER).unwrap();
        // Default engine (system-bash) carries no stdin payload — the
        // wrapped command rides argv exactly as pre-#1143.
        assert!(prepared.stdin_payload.is_none());
        // The command should be a bash process
        let inner = prepared.cmd.as_std();
        #[cfg(unix)]
        assert_eq!(inner.get_program(), "bash");
        #[cfg(windows)]
        {
            // Post-#1121: never bare "bash" (the WSL trap) and never a
            // System32/Sysnative path. On a Windows host without Git Bash,
            // build_command errors instead — the unwrap above is the assert.
            let program = inner.get_program().to_string_lossy().into_owned();
            assert!(
                program.to_ascii_lowercase().ends_with("bash.exe"),
                "expected a concrete bash.exe path, got: {program}"
            );
            assert!(
                !is_under_windows_system32(Path::new(&program)),
                "resolved shell must never live under System32/Sysnative: {program}"
            );
        }
        let args: Vec<_> = inner.get_args().collect();
        assert_eq!(args[0], "-c");
        // The second arg should contain our command and the pwd marker
        let wrapped = args[1].to_str().unwrap();
        assert!(wrapped.contains("echo hello"));
        assert!(wrapped.contains(TEST_MARKER));
    }

    // ── Builtin shell engine (#1143) ──────────────────────────────────────
    //
    // These exercise `build_command_for_engine` directly with an explicit
    // engine so the process-global `SHELL_ENGINE` OnceLock is never
    // mutated (tests share one process). The full spawn path cannot be
    // unit-tested end-to-end here because `current_exe()` inside `cargo
    // test` is the *test* binary, not `alms` — covering that path is the
    // documented validation follow-up on #1143.

    #[test]
    fn test_build_command_builtin_reexecs_current_exe() {
        let dir = tempfile::tempdir().unwrap();
        let prepared = build_command_for_engine(
            ShellEngine::Builtin,
            "echo hello",
            dir.path(),
            None,
            true,
            TEST_MARKER,
        )
        .unwrap();

        let inner = prepared.cmd.as_std();
        // Zero PATH resolution: the program must be exactly current_exe().
        assert_eq!(
            PathBuf::from(inner.get_program()),
            std::env::current_exe().unwrap()
        );
        // Single argv entry: the hidden subcommand. The command string is
        // NOT on argv...
        let args: Vec<_> = inner.get_args().collect();
        assert_eq!(args, ["shell-host"]);
        // ...it travels via stdin, wrapped with the pwd marker exactly
        // like the bash -c path.
        let payload = prepared
            .stdin_payload
            .expect("builtin engine pipes the command");
        assert_eq!(
            payload,
            wrap_command_with_pwd_marker("echo hello", TEST_MARKER)
        );
    }

    #[tokio::test]
    async fn test_stdin_write_is_covered_by_operator_timeout() {
        // S1 (PR #1144 review): a child that never drains its stdin must not
        // let the stdin write block past the operator deadline. The payload
        // is far larger than any OS pipe buffer (~64 KiB), so `write_all`
        // genuinely blocks once the buffer fills — the single timeout around
        // the whole interaction is what gets us out, and `kill_on_drop`
        // reaps the child when the interaction future is dropped.
        let mut cmd = if cfg!(windows) {
            // `ping -n 30` keeps stdin open without reading it for ~29s.
            let mut c = tokio::process::Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = tokio::process::Command::new("sleep");
            c.arg("30");
            c
        };
        cmd.stdin(std::process::Stdio::piped());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let child = cmd.spawn().expect("spawn non-draining child");
        // 4 MiB — far beyond any platform's pipe buffer.
        let payload = "x".repeat(4 * 1024 * 1024);

        let started = std::time::Instant::now();
        let result = drive_child_to_completion(child, Some(payload), 1_000).await;
        let elapsed = started.elapsed();

        let err = result.expect_err("write to a non-draining child must time out");
        assert!(
            err.to_string().contains("timed out"),
            "expected the operator-timeout error, got: {err}"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(15),
            "timeout must fire at the deadline, not at child exit: {elapsed:?}"
        );
    }

    #[test]
    fn test_build_command_engine_dispatch_default_is_system_bash() {
        // Without init_shell_resolution ever being called (library users,
        // tests), the engine must default to system-bash — the additive
        // guarantee of #1143.
        assert_eq!(shell_engine(), ShellEngine::SystemBash);
    }

    // ── Shell binary resolution (#1121) ──────────────────────────────────
    //
    // These tests exercise the pure resolution core (`resolve_shell_path`,
    // `bash_candidates`, `is_under_windows_system32`) with injected
    // filesystem probes, so they run on every platform without touching the
    // live filesystem or environment. Path fixtures use forward slashes and
    // `join()`-built expectations so they compare equal under both Windows
    // and Unix `Path` semantics.

    #[test]
    fn test_is_under_windows_system32() {
        // The WSL launcher's home, in its common spellings.
        assert!(is_under_windows_system32(Path::new(
            "C:\\Windows\\System32\\bash.exe"
        )));
        assert!(is_under_windows_system32(Path::new(
            "C:/Windows/System32/bash.exe"
        )));
        assert!(is_under_windows_system32(Path::new(
            "C:\\WINDOWS\\SYSTEM32"
        )));
        // Sysnative (the 32-bit-process alias for the 64-bit System32).
        assert!(is_under_windows_system32(Path::new(
            "C:\\Windows\\Sysnative\\bash.exe"
        )));
        // Verbatim prefix and nested paths are still caught.
        assert!(is_under_windows_system32(Path::new(
            "\\\\?\\C:\\Windows\\System32\\bash.exe"
        )));
        assert!(is_under_windows_system32(Path::new(
            "C:\\Windows\\System32\\wsl\\bash.exe"
        )));
        // Legitimate locations must not match.
        assert!(!is_under_windows_system32(Path::new(
            "C:\\Program Files\\Git\\bin\\bash.exe"
        )));
        assert!(!is_under_windows_system32(Path::new(
            "D:\\tools\\PortableGit\\usr\\bin\\bash.exe"
        )));
        // Substring red herring: "system32" as a name *prefix* is not the dir.
        assert!(!is_under_windows_system32(Path::new(
            "C:\\system32fake\\bash.exe"
        )));
        assert!(!is_under_windows_system32(Path::new("/usr/bin/bash")));
    }

    #[test]
    fn test_bash_candidates_git_derived_ordering() {
        // Static well-knowns come first; git-derived candidates follow in
        // bin → usr/bin order, rooted at the parent of git.exe's directory
        // (<GitRoot>/cmd/git.exe → <GitRoot>/bin/bash.exe etc.).
        let git_root = Path::new("D:/tools/PortableGit");
        let statics = vec![Path::new("C:/Program Files/Git/bin/bash.exe").to_path_buf()];
        let git_dir = git_root.join("cmd");

        let candidates = bash_candidates(&statics, Some(&git_dir));

        assert_eq!(
            candidates,
            vec![
                Path::new("C:/Program Files/Git/bin/bash.exe").to_path_buf(),
                git_root.join("bin").join("bash.exe"),
                git_root.join("usr").join("bin").join("bash.exe"),
            ]
        );
    }

    #[test]
    fn test_bash_candidates_dedupes_git_derived_against_statics() {
        // A standard install: git.exe found at <ProgramFiles>/Git/cmd, whose
        // derived candidates already exist in the static list. No dupes.
        let git_root = Path::new("C:/Program Files/Git");
        let statics = vec![
            git_root.join("bin").join("bash.exe"),
            git_root.join("usr").join("bin").join("bash.exe"),
        ];
        let git_dir = git_root.join("cmd");

        let candidates = bash_candidates(&statics, Some(&git_dir));
        assert_eq!(candidates, statics);
    }

    #[test]
    fn test_bash_candidates_filters_system32_entries() {
        // Even a poisoned static list or a System32-derived git root must
        // never produce a System32 candidate.
        let statics = vec![
            Path::new("C:\\Windows\\System32\\bash.exe").to_path_buf(),
            Path::new("C:/Program Files/Git/bin/bash.exe").to_path_buf(),
        ];
        let candidates = bash_candidates(&statics, None);
        assert_eq!(
            candidates,
            vec![Path::new("C:/Program Files/Git/bin/bash.exe").to_path_buf()]
        );
    }

    #[test]
    fn test_resolve_shell_path_override_wins_over_candidates() {
        // Both the override and a discovery candidate "exist" — the
        // operator's [tools].shell_path must win unconditionally.
        let override_path = Path::new("D:/custom/bash.exe");
        let candidates = vec![Path::new("C:/Program Files/Git/bin/bash.exe").to_path_buf()];

        let resolved =
            resolve_shell_path(Some(override_path), &candidates, |_: &Path| true).unwrap();
        assert_eq!(resolved, override_path);
    }

    #[test]
    fn test_resolve_shell_path_missing_override_is_hard_error() {
        // Override set but pointing at a non-file: hard error, NOT a silent
        // fall-through to discovery (a candidate that exists must not win).
        let override_path = Path::new("D:/custom/bash.exe");
        let candidates = vec![Path::new("C:/Program Files/Git/bin/bash.exe").to_path_buf()];

        let err = resolve_shell_path(Some(override_path), &candidates, |p: &Path| {
            p != override_path
        })
        .unwrap_err();
        assert!(
            err.contains("shell_path"),
            "error must name the misconfigured knob: {err}"
        );
    }

    #[test]
    fn test_resolve_shell_path_system32_override_rejected() {
        // Operator-supplied paths get the same System32/Sysnative hard
        // reject as discovered candidates — even when the file exists.
        let override_path = Path::new("C:\\Windows\\System32\\bash.exe");
        let err = resolve_shell_path(Some(override_path), &[], |_: &Path| true).unwrap_err();
        assert!(
            err.contains("WSL"),
            "error must explain the WSL-launcher rejection: {err}"
        );
    }

    #[test]
    fn test_resolve_shell_path_first_existing_candidate_wins() {
        let first = Path::new("C:/Program Files/Git/bin/bash.exe").to_path_buf();
        let second = Path::new("D:/tools/PortableGit/bin/bash.exe").to_path_buf();
        let candidates = vec![first.clone(), second.clone()];

        // Only the second exists → it wins.
        let resolved = resolve_shell_path(None, &candidates, |p: &Path| p == second).unwrap();
        assert_eq!(resolved, second);

        // Both exist → ordering decides.
        let resolved = resolve_shell_path(None, &candidates, |_: &Path| true).unwrap();
        assert_eq!(resolved, first);
    }

    #[test]
    fn test_resolve_shell_path_skips_system32_candidate_even_if_present() {
        // Belt-and-suspenders inside the resolver itself: a System32
        // candidate that sneaks past `bash_candidates` is still skipped.
        let trap = Path::new("C:\\Windows\\System32\\bash.exe").to_path_buf();
        let good = Path::new("C:/Program Files/Git/bin/bash.exe").to_path_buf();
        let candidates = vec![trap, good.clone()];

        let resolved = resolve_shell_path(None, &candidates, |_: &Path| true).unwrap();
        assert_eq!(resolved, good);
    }

    #[test]
    fn test_resolve_shell_path_not_found_is_actionable_error() {
        // Nothing resolves: the result is a loud, actionable error — never
        // a bare-"bash" fallback (the pre-#1121 silent-WSL failure mode).
        let candidates = vec![Path::new("C:/nope/Git/bin/bash.exe").to_path_buf()];
        let err = resolve_shell_path(None, &candidates, |_: &Path| false).unwrap_err();

        assert!(
            err.contains("Git for Windows"),
            "error must tell the operator how to fix it (install Git): {err}"
        );
        assert!(
            err.contains("[tools].shell_path"),
            "error must mention the config-knob escape hatch: {err}"
        );
        assert!(
            err.contains("WSL"),
            "error must explain why bare 'bash' is refused: {err}"
        );
    }

    #[test]
    fn test_validate_cwd_inside_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let cwd = root.join("subdir");
        std::fs::create_dir_all(&cwd).unwrap();
        assert!(validate_cwd(&cwd, &root).is_ok());
    }

    #[test]
    fn test_validate_cwd_outside_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        #[cfg(unix)]
        {
            assert!(validate_cwd(Path::new("/etc"), &root).is_err());
        }
        #[cfg(windows)]
        {
            assert!(validate_cwd(Path::new("C:\\Windows"), &root).is_err());
        }
    }

    /// #1255 — the sandbox root must contain itself.
    ///
    /// The pre-fix comparison could reject the root when the two sides were
    /// spelled differently, which is exactly what the 15 production warnings
    /// were: `new_cwd = /c/dev/alms-test-workspace` against
    /// `sandbox_root = \\?\C:\dev\alms-test-workspace`.
    #[test]
    fn validate_cwd_accepts_the_root_itself() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        assert!(
            validate_cwd(&root, &root).is_ok(),
            "the sandbox root can never be outside itself"
        );
    }

    /// #1255 — an un-canonicalised root (no `\\?\` prefix on Windows) must
    /// still match a canonicalised cwd.
    #[test]
    fn validate_cwd_matches_across_canonicalisation_forms() {
        let dir = tempfile::tempdir().unwrap();
        let raw_root = dir.path().to_path_buf();
        let canonical_root = std::fs::canonicalize(&raw_root).unwrap();
        let nested = canonical_root.join(".alms").join("tool-output");
        std::fs::create_dir_all(&nested).unwrap();

        // Raw root vs canonical nested path, and the reverse pairing.
        assert!(validate_cwd(&nested, &raw_root).is_ok());
        assert!(validate_cwd(&raw_root, &canonical_root).is_ok());
    }

    /// #1255 — the returned path must be usable as a real working directory,
    /// not the raw string the shell reported.
    #[test]
    fn validate_cwd_returns_a_resolvable_path() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();

        let resolved = validate_cwd(&nested, &root).expect("nested dir is inside the root");
        assert!(
            resolved.is_dir(),
            "validate_cwd must hand back a directory that exists: {}",
            resolved.display()
        );
    }

    /// #1262 — a rejection has two possible causes and the daemon may only
    /// report the one it actually established. This is the case where it
    /// *did*: a real directory, successfully canonicalised, genuinely not
    /// under the root.
    #[test]
    fn check_cwd_reports_a_resolved_escape_as_outside_the_root() {
        let root_dir = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root_dir.path()).unwrap();
        let outside = std::fs::canonicalize(outside_dir.path()).unwrap();

        assert_eq!(
            check_cwd(&outside, &root),
            Err(CwdRejection::OutsideRoot),
            "both sides resolved, so the escape is an established fact"
        );
    }

    /// #1262 — and this is the case where it did *not*. `normalise` falls
    /// back to the raw input when `canonicalize` fails, so an unresolvable
    /// path is rejected by the same `is_within` comparison as a real escape.
    /// Failing closed is correct; claiming the path was outside the root is
    /// not, because nothing here determined where it is.
    ///
    /// This is the shape of #1266, where Windows Git Bash reports `%TEMP%`
    /// through its `/tmp` mount and *every* command lands on this branch.
    #[test]
    fn check_cwd_reports_an_unresolvable_cwd_as_not_verifiable() {
        let root_dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root_dir.path()).unwrap();
        let unresolvable = Path::new("/alms-1262-no-such-directory/nested");

        assert!(
            std::fs::canonicalize(unresolvable).is_err(),
            "fixture precondition: the path must not resolve"
        );
        assert_eq!(
            check_cwd(unresolvable, &root),
            Err(CwdRejection::NotVerifiable),
            "an unresolved path supports 'not confirmed inside', not 'outside'"
        );
    }

    /// The reason reaches the operator-facing error too — the pre-command
    /// check words its message from the same classification.
    #[test]
    fn validate_cwd_message_matches_the_established_cause() {
        let root_dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root_dir.path()).unwrap();
        let err = validate_cwd(Path::new("/alms-1262-no-such-directory"), &root)
            .expect_err("an unresolvable cwd must still be refused");

        let message = err.to_string();
        assert!(
            message.contains("could not be confirmed inside"),
            "the error must not overstate what was determined; got: {message}"
        );
    }

    /// A sibling directory whose name merely shares a string prefix with the
    /// root is still an escape.
    ///
    /// Standing guard, not a #1255 regression test: `Path::starts_with` was
    /// already component-wise, so this never failed. It is here to keep the
    /// new component comparison from regressing into a string one.
    #[test]
    fn validate_cwd_rejects_string_prefix_sibling() {
        let parent = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(parent.path()).unwrap().join("ws");
        let sibling = std::fs::canonicalize(parent.path())
            .unwrap()
            .join("ws-evil");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();

        assert!(
            validate_cwd(&sibling, &root).is_err(),
            "'ws-evil' shares a string prefix with 'ws' but is a different directory"
        );
    }

    /// #1255 — the MSYS / Git-Bash form that the shell engine actually
    /// reports on Windows must be accepted for the root and for a nested
    /// path, and a genuine escape must still be rejected.
    ///
    /// Windows-only: on Unix `/c/...` is an ordinary absolute path and is
    /// deliberately never reinterpreted as a drive. The pure string
    /// transform behind this is covered cross-platform in
    /// [`super::super::pathnorm`], which is what CI (Linux) exercises.
    #[cfg(windows)]
    #[test]
    fn validate_cwd_accepts_msys_form_reported_by_git_bash() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let nested = root.join(".alms").join("tool-output");
        std::fs::create_dir_all(&nested).unwrap();

        // Build the MSYS spelling the way Git-Bash's `pwd` would report it:
        // `C:\dev\ws` -> `/c/dev/ws`.
        let to_msys = |p: &Path| -> PathBuf {
            let s = super::super::pathnorm::strip_verbatim_prefix(p)
                .to_string_lossy()
                .to_string();
            let (drive, tail) = s.split_at(2);
            PathBuf::from(format!(
                "/{}{}",
                drive[..1].to_ascii_lowercase(),
                tail.replace('\\', "/")
            ))
        };

        let msys_root = to_msys(&root);
        let msys_nested = to_msys(&nested);

        assert!(
            validate_cwd(&msys_root, &root).is_ok(),
            "MSYS-form root must be recognised as the root: {}",
            msys_root.display()
        );
        assert!(
            validate_cwd(&msys_nested, &root).is_ok(),
            "MSYS-form nested path must be recognised as inside the root: {}",
            msys_nested.display()
        );
        // A real escape is still an escape in MSYS form.
        assert!(
            validate_cwd(Path::new("/c/Windows"), &root).is_err(),
            "MSYS-form escape must still be rejected"
        );
    }

    /// #1255 — a differently-cased spelling of the root must still contain
    /// the same directory on Windows.
    ///
    /// Note what this does and does not pin. It is an end-to-end acceptance
    /// check, and it passes with `pathnorm::FOLD_CASE` forced to `false`:
    /// `std::fs::canonicalize` is `GetFinalPathNameByHandle`, which returns
    /// the on-disk casing, so both sides have already been case-normalised
    /// before the comparison runs. Case folding is only load-bearing where
    /// canonicalisation did not happen. The folding *policy* is pinned
    /// separately and directly, by `pathnorm`'s
    /// `fold_case_policy_tracks_the_platform` and
    /// `case_folding_matches_only_when_enabled`.
    #[cfg(windows)]
    #[test]
    fn validate_cwd_accepts_a_differently_cased_root_spelling_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let nested = root.join("SubDir");
        std::fs::create_dir_all(&nested).unwrap();

        let upper = PathBuf::from(root.to_string_lossy().to_uppercase());
        assert!(
            validate_cwd(&nested, &upper).is_ok(),
            "an upper-cased root names the same directory on Windows"
        );
    }
}
