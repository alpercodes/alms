// SPDX-License-Identifier: Apache-2.0

//! What the `alms` binary does with an `alms.toml` that exists but won't parse.
//!
//! Commands that act on the configuration must refuse it. Running on compiled
//! defaults would discard `shell_policy` and `[security]`, send runs to the
//! default provider, and open a different database. Commands that never use the
//! configuration must keep working, so a broken file in the current directory
//! can't break `alms completions` or `alms health`.

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BROKEN_TOML: &str = "[tools]\nshell_policy = \"workspace_only\"\n\n[llm\n";

fn alms(cwd: &Path, home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_alms"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Windows can't create sockets without it.
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    command
}

/// Wait for the command, killing it if it is still running at the deadline. A
/// gateway that wrongly starts would otherwise hang the test.
fn run_with_deadline(mut command: Command) -> Output {
    let mut child = command.spawn().expect("spawn alms");
    let deadline = Instant::now() + Duration::from_secs(20);
    while child.try_wait().expect("poll alms").is_none() {
        if Instant::now() > deadline {
            child.kill().expect("kill alms");
            let output = child.wait_with_output().expect("collect alms output");
            panic!(
                "alms was still running at the deadline, so it did not refuse the file. \
                 stderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().expect("collect alms output")
}

fn broken_config_dir() -> (tempfile::TempDir, tempfile::TempDir) {
    let cwd = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(cwd.path().join("alms.toml"), BROKEN_TOML).expect("write alms.toml");
    (cwd, home)
}

fn assert_refused_naming_file_and_problem(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(stderr.contains("alms.toml"), "must name the file: {stderr}");
    assert!(
        stderr.contains("TOML parse error"),
        "must state the parse problem: {stderr}"
    );
    assert!(
        !stderr.contains("Using defaults"),
        "must not fall back: {stderr}"
    );
}

#[test]
fn gateway_refuses_an_unparseable_config_file() {
    let (cwd, home) = broken_config_dir();
    let output = run_with_deadline(alms(
        cwd.path(),
        home.path(),
        &["gateway", "--bind", "127.0.0.1:0"],
    ));
    assert_refused_naming_file_and_problem(&output);
}

/// These open the database at the configured path, so defaults would mean
/// silently using a different database.
#[test]
fn commands_that_use_the_config_refuse_it_too() {
    for args in [
        &["agent", "list"][..],
        &["session", "list"][..],
        &["job", "list"][..],
        &["job", "show", "00000000-0000-0000-0000-000000000000"][..],
        // `--agent` resolves the name in the local database before any request.
        &[
            "run",
            "create",
            "--agent",
            "x",
            "--session",
            "00000000-0000-0000-0000-000000000000",
            "--input",
            "hi",
            "--url",
            "http://127.0.0.1:9",
        ][..],
        &["auth", "list"][..],
    ] {
        let (cwd, home) = broken_config_dir();
        let output = run_with_deadline(alms(cwd.path(), home.path(), args));
        assert_refused_naming_file_and_problem(&output);
        assert!(
            !cwd.path().join(".alms").exists(),
            "{args:?} must not create a default data directory"
        );
    }
}

#[test]
fn commands_that_do_not_use_the_config_still_work() {
    let (cwd, home) = broken_config_dir();
    let completions = run_with_deadline(alms(cwd.path(), home.path(), &["completions", "zsh"]));
    assert!(
        completions.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&completions.stderr)
    );

    // Port 9 on loopback: nothing answers, so this fails on the connection and
    // not on the configuration.
    let health = run_with_deadline(alms(
        cwd.path(),
        home.path(),
        &["health", "--url", "http://127.0.0.1:9"],
    ));
    let stderr = String::from_utf8_lossy(&health.stderr);
    assert!(
        !stderr.contains("TOML parse error"),
        "health must not read the config: {stderr}"
    );

    // The pre-flight probe fails on the connection before any browser opens.
    let dashboard = run_with_deadline(alms(
        cwd.path(),
        home.path(),
        &["dashboard", "--url", "http://127.0.0.1:9"],
    ));
    let stderr = String::from_utf8_lossy(&dashboard.stderr);
    assert!(
        !stderr.contains("TOML parse error"),
        "dashboard must not read the config: {stderr}"
    );
}

/// With no `./alms.toml`, a broken home config is the file in use, so the
/// message must name it, and must not send the operator to `alms.toml` or call
/// a CLI command "the daemon".
#[test]
fn a_broken_home_config_is_named_in_the_error() {
    let cwd = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let dir = home.path().join(".config").join("alms");
    std::fs::create_dir_all(&dir).expect("mkdir");
    std::fs::write(dir.join("config.toml"), BROKEN_TOML).expect("write config.toml");

    let output = run_with_deadline(alms(cwd.path(), home.path(), &["agent", "list"]));

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "stderr: {stderr}");
    assert!(
        stderr.contains("config.toml"),
        "must name the file: {stderr}"
    );
    assert!(!stderr.contains("alms.toml"), "wrong file named: {stderr}");
    assert!(!stderr.contains("daemon"), "not a daemon: {stderr}");
}

#[test]
fn a_missing_config_file_is_still_fine() {
    let cwd = tempfile::tempdir().expect("tempdir");
    let home = tempfile::tempdir().expect("tempdir");
    let output = run_with_deadline(alms(cwd.path(), home.path(), &["agent", "list"]));
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
