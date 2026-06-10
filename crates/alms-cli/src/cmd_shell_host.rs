//! Hidden `alms shell-host` subcommand — the self-contained shell engine
//! (#1143, Phase 2 of #1121).
//!
//! Under `[tools].shell_engine = "builtin"`, the daemon's shell tool
//! re-execs the ALMS binary itself with this subcommand instead of
//! resolving an external bash. The command string arrives on **stdin**
//! (read to EOF) rather than argv, sidestepping platform quoting rules and
//! command-line length limits. The string — already wrapped with the
//! sandbox's pwd marker by `alms-sandbox::shell::exec::build_command` — is
//! evaluated by the `brush_core` Rust bash interpreter. stdout/stderr flow
//! straight through this process's own handles (which the daemon pipes),
//! and the shell's exit code becomes the process exit code.
//!
//! Staying a *child process* is the point of the design (Tim's research on
//! #1121, option C′): Landlock `pre_exec`, `kill_on_drop`, timeouts, env
//! scrubbing, the pwd-marker wrapper, the destructive-command classifier,
//! and operator `shell_permissions` all keep working unchanged because the
//! daemon still spawns a sandboxed child evaluating bash syntax. Only *how*
//! that child is located changed: `current_exe()` instead of a bash finder,
//! so the silent-WSL hazard is impossible by construction.
//!
//! Note brush interprets bash *syntax* but does not bundle coreutils —
//! `grep`/`sed`/`tail`/... remain external commands the shell spawns from
//! `PATH` like any bash would.

use anyhow::Context;
use brush_builtins::{BuiltinSet, ShellBuilderExt};
use brush_core::openfiles::OpenFile;
use brush_core::{ProfileLoadBehavior, RcLoadBehavior, ShellFd};
use std::collections::HashMap;

/// Exit code when the host itself fails before or while evaluating —
/// mirrors the POSIX shell convention for "command invoked cannot execute".
/// Ordinary command failures (including parse errors, which brush reports
/// on stderr and converts to a nonzero status) do NOT take this path.
const HOST_FAILURE_EXIT_CODE: i32 = 126;

/// Entry point for the hidden subcommand. Reads the command from stdin,
/// evaluates it, and returns the process exit code to hand to
/// [`std::process::exit`].
///
/// Never panics on bad input: any host-level failure prints a diagnostic
/// to stderr (which the daemon surfaces as the tool's stderr) and maps to
/// [`HOST_FAILURE_EXIT_CODE`].
pub(crate) async fn run() -> i32 {
    let command = match std::io::read_to_string(std::io::stdin().lock()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("alms shell-host: failed to read command from stdin: {e}");
            return HOST_FAILURE_EXIT_CODE;
        }
    };

    match eval_command(&command, None).await {
        Ok(code) => i32::from(code),
        Err(e) => {
            eprintln!("alms shell-host: {e:#}");
            HOST_FAILURE_EXIT_CODE
        }
    }
}

/// Evaluate `command` through a non-interactive brush shell, returning the
/// shell's exit code.
///
/// `fds` optionally overrides the shell's standard streams — used by the
/// unit tests to capture stdout through a pipe. `None` (the production
/// path) leaves them attached to this process's own stdin/stdout/stderr,
/// which the daemon has piped.
///
/// Shell construction is deterministic: profile and rc loading are
/// explicitly skipped so agent shell behavior can never depend on operator
/// dotfiles, and the full bash-mode builtin set is registered (`echo`,
/// `pwd`, `cd`, `exit`, `export`, `test`, ...).
async fn eval_command(
    command: &str,
    fds: Option<HashMap<ShellFd, OpenFile>>,
) -> anyhow::Result<u8> {
    // The two arms duplicate the builder chain because each bon-builder
    // setter transitions the builder's typestate — the arms have different
    // types and only unify after `.build()`.
    let mut shell = match fds {
        Some(fds) => {
            brush_core::Shell::builder()
                .default_builtins(BuiltinSet::BashMode)
                .profile(ProfileLoadBehavior::Skip)
                .rc(RcLoadBehavior::Skip)
                .fds(fds)
                .build()
                .await
        }
        None => {
            brush_core::Shell::builder()
                .default_builtins(BuiltinSet::BashMode)
                .profile(ProfileLoadBehavior::Skip)
                .rc(RcLoadBehavior::Skip)
                .build()
                .await
        }
    }
    .context("failed to construct brush shell")?;

    // `run_dash_c_command` is brush's `bash -c` equivalent: it evaluates
    // the whole string as one program (parse errors become a nonzero exit
    // status with a diagnostic on stderr, exactly like bash -c) and runs
    // shell exit handling afterwards.
    let result = shell
        .run_dash_c_command(command)
        .await
        .context("brush shell evaluation failed")?;

    let code = u8::from(result.exit_code);

    // Drop the shell before returning so any fds handed to it (e.g. the
    // test harness's pipe writer) are closed and readers observe EOF.
    drop(shell);

    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use brush_core::openfiles::OpenFiles;
    use std::io::Read;

    /// Test harness: evaluate `command` with stdout captured through an
    /// anonymous pipe, returning `(stdout, exit_code)`.
    ///
    /// This exercises the exact evaluation path `run()` uses in
    /// production — only fd 1's destination differs. The full re-exec
    /// spawn path (`alms shell-host` as a real child process) cannot be
    /// unit-tested here because `current_exe()` inside `cargo test` is
    /// the test binary, not `alms`; see #1143 for the validation
    /// follow-up.
    async fn eval_captured(command: &str) -> (String, u8) {
        let (reader, writer) = std::io::pipe().expect("anonymous pipe");

        // Drain the pipe on a thread so a chatty command can't deadlock
        // against a full pipe buffer while the shell is still running.
        let reader_thread = std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = String::new();
            reader.read_to_string(&mut buf).expect("read pipe");
            buf
        });

        let fds = HashMap::from([
            (OpenFiles::STDOUT_FD, OpenFile::from(writer)),
            // Discard stderr so expected diagnostics (e.g. the parse-error
            // test) don't pollute the test runner's output.
            (
                OpenFiles::STDERR_FD,
                brush_core::openfiles::null().expect("null sink"),
            ),
        ]);
        let exit = eval_command(command, Some(fds)).await.expect("eval");

        // eval_command dropped the shell (and with it the pipe writer),
        // so the reader thread sees EOF and finishes.
        let stdout = reader_thread.join().expect("join reader thread");
        (stdout, exit)
    }

    #[tokio::test]
    async fn echo_works() {
        let (out, code) = eval_captured("echo hello").await;
        assert_eq!(out, "hello\n");
        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn pipeline_works() {
        // Builtin-to-builtin pipeline so the test has no external-binary
        // (coreutils-on-PATH) dependency on any CI platform.
        let (out, code) =
            eval_captured("printf 'a\\nb\\n' | while read x; do echo \"got:$x\"; done").await;
        assert_eq!(out, "got:a\ngot:b\n");
        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn and_and_short_circuits_and_dollar_question_works() {
        let (out, code) = eval_captured("false && echo nope; echo \"after:$?\"").await;
        // `false && ...` short-circuits; $? observes the failed && list.
        assert_eq!(out, "after:1\n");
        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn nonzero_exit_code_propagates() {
        let (_, code) = eval_captured("exit 42").await;
        assert_eq!(code, 42);
    }

    #[tokio::test]
    async fn parse_error_is_nonzero_not_host_failure() {
        // Unparseable input must behave like bash -c: nonzero exit status,
        // not an Err out of eval_command.
        let (_, code) = eval_captured("if then fi (").await;
        assert_ne!(code, 0);
    }

    #[tokio::test]
    async fn output_redirect_works() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("out.txt");
        // Forward slashes work for bash semantics on every platform.
        let target_str = target.display().to_string().replace('\\', "/");

        let (_, code) = eval_captured(&format!("echo redirected > '{target_str}'")).await;
        assert_eq!(code, 0);
        let written = std::fs::read_to_string(&target).expect("redirect target written");
        assert_eq!(written, "redirected\n");
    }

    #[tokio::test]
    async fn pwd_marker_wrapper_sequence_works() {
        // The exact tail `alms-sandbox`'s wrap_command_with_pwd_marker
        // appends: `;` sequencing, `$?` capture, bare `echo`, quoted
        // marker echo, `pwd`, and `exit $__alms_ec` must all evaluate —
        // and the *user command's* exit code must survive the tail.
        const MARKER: &str = "__ALMS_PWD_TEST_MARKER__";
        let wrapped =
            format!("echo hi; false; __alms_ec=$?; echo; echo '{MARKER}'; pwd; exit $__alms_ec");

        let (out, code) = eval_captured(&wrapped).await;
        assert_eq!(
            code, 1,
            "user command's exit code must survive the wrapper tail"
        );

        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "hi");
        assert_eq!(lines[1], "", "bare `echo` separator line");
        assert_eq!(lines[2], MARKER);
        assert!(
            !lines[3].trim().is_empty(),
            "pwd must print a non-empty cwd line after the marker: {out:?}"
        );
    }

    #[tokio::test]
    async fn cd_is_reflected_by_pwd() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target_str = dir.path().display().to_string().replace('\\', "/");
        let leaf = dir
            .path()
            .file_name()
            .expect("tempdir leaf")
            .to_string_lossy()
            .into_owned();

        let (out, code) = eval_captured(&format!("cd '{target_str}' && pwd")).await;
        assert_eq!(code, 0);
        // Lenient cross-platform assertion: path spelling differs between
        // Windows and Unix, but the unique tempdir leaf must appear.
        assert!(
            out.contains(&leaf),
            "pwd after cd must show the target dir (leaf {leaf:?}): {out:?}"
        );
    }
}
