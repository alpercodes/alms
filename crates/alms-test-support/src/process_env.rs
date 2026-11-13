// SPDX-License-Identifier: Apache-2.0

//! The one way tests touch process-global state: environment variables and
//! the working directory.
//!
//! # Why it is shared
//!
//! `cargo test` runs a crate's tests on parallel threads of **one process**,
//! so an env var or the working directory set by one test is what every
//! other test in that binary reads until it is put back. Before this module
//! each crate kept its own mutex, and some kept two (`ENV_LOCK` and
//! `BUDGET_ENV_LOCK` in `alms-core`, two `ENV_LOCK`s in `alms-runtime`),
//! each "guarding a disjoint var-set". That contract held only as long as
//! nobody read a var under the other lock, or the working directory under
//! neither, and it did not: `secrets.rs` resolved paths against a working
//! directory a config test had moved, and gateway tests that expect strict
//! budget validation read `ALMS_LLM_BUDGET_VALIDATION` while another test
//! held it at `warn`. Here there is **one lock per test binary** (each test
//! binary links its own copy of this crate's statics), covering every var
//! and the working directory.
//!
//! # Why the lock survives a panic
//!
//! A test that panics while holding a `std::sync::Mutex` poisons it, and
//! `lock().unwrap()` then panics in every later test that wants it: one real
//! failure surfaced as ten to fifty unrelated `PoisonError` failures, in a
//! different set each run, which buried the real one and read as flakiness.
//! [`lock`] recovers the guard from a poisoned mutex instead. That is sound
//! here because the state the lock protects is put back by drop guards
//! ([`VarGuard`], [`CurrentDirGuard`]) that run during unwinding, before the
//! lock guard (declared first, so dropped last) is released.
//!
//! # Rules
//!
//! * Take [`lock`] first, then the guards, as locals in that order.
//! * Change env vars and the working directory only through this module.
//!   `every_env_and_cwd_write_goes_through_process_env` (below) fails on a
//!   `set_var` / `remove_var` / `set_current_dir` anywhere else in the
//!   workspace.
//! * A test that **reads** state another test changes, e.g. one that
//!   compares a path against `std::env::current_dir()`, takes [`lock`] too.
//!   Prefer not reading or writing process state at all where the code
//!   under test can be handed the value instead.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

static LOCK: Mutex<()> = Mutex::new(());

/// Exclusive access to this test binary's environment and working directory,
/// recovered rather than propagated if a previous holder panicked. See the
/// module docs for why that is sound.
pub fn lock() -> MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Set an env var for the rest of the process. Hold [`lock`] and prefer
/// [`VarGuard::set`], which puts the old value back.
pub fn set_var(key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
    // SAFETY: env writes race with reads on other threads. Every test that
    // writes or reads shared env state holds `lock()` (module rules), and the
    // workspace guard test keeps writes from happening anywhere else.
    unsafe { std::env::set_var(key, value) }
}

/// Remove an env var for the rest of the process. Hold [`lock`] and prefer
/// [`VarGuard::remove`].
pub fn remove_var(key: impl AsRef<OsStr>) {
    // SAFETY: as for `set_var`.
    unsafe { std::env::remove_var(key) }
}

/// Sets or removes one env var and puts its previous value back on drop,
/// including when the test panics.
#[must_use = "the variable is restored when the guard is dropped"]
pub struct VarGuard {
    key: OsString,
    original: Option<OsString>,
}

impl VarGuard {
    pub fn set(key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        let guard = Self::capture(key.as_ref());
        set_var(key, value);
        guard
    }

    pub fn remove(key: impl AsRef<OsStr>) -> Self {
        let guard = Self::capture(key.as_ref());
        remove_var(key);
        guard
    }

    fn capture(key: &OsStr) -> Self {
        Self {
            key: key.to_owned(),
            original: std::env::var_os(key),
        }
    }
}

impl Drop for VarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => set_var(&self.key, value),
            None => remove_var(&self.key),
        }
    }
}

/// Changes the working directory and changes it back on drop, including
/// when the test panics.
#[must_use = "the working directory is restored when the guard is dropped"]
pub struct CurrentDirGuard {
    original: PathBuf,
}

impl CurrentDirGuard {
    pub fn set(dir: impl AsRef<Path>) -> Self {
        let original = std::env::current_dir().expect("the working directory is readable");
        std::env::set_current_dir(dir.as_ref()).expect("change the working directory");
        Self { original }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        // Not `expect`: a panic here, during another panic's unwinding,
        // would abort the whole test binary.
        let _ = std::env::set_current_dir(&self.original);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VAR: &str = "ALMS_TEST_SUPPORT_PROCESS_ENV_PROBE";

    /// The failure mode this module exists for: a test panics while holding
    /// the lock with a variable and the working directory changed. The next
    /// holder must get the lock (not a `PoisonError`) and find both put back.
    #[test]
    fn a_panic_while_locked_leaves_the_lock_usable_and_the_state_restored() {
        let cwd_before = std::env::current_dir().unwrap();
        let elsewhere = std::env::temp_dir();

        let joined = std::thread::spawn(move || {
            let _env = lock();
            let _var = VarGuard::set(VAR, "set-by-the-panicking-test");
            let _cwd = CurrentDirGuard::set(&elsewhere);
            panic!("deliberate panic while holding the process-env lock");
        })
        .join();
        assert!(joined.is_err(), "the helper thread must have panicked");
        assert!(LOCK.is_poisoned(), "precondition: the mutex is poisoned");

        let _env = lock();
        assert_eq!(std::env::var_os(VAR), None, "the variable was put back");
        assert_eq!(std::env::current_dir().unwrap(), cwd_before);
    }

    #[test]
    fn a_var_guard_restores_a_previous_value() {
        let _env = lock();
        let _outer = VarGuard::set(VAR, "outer");
        {
            let _inner = VarGuard::set(VAR, "inner");
            assert_eq!(std::env::var(VAR).unwrap(), "inner");
            let _gone = VarGuard::remove(VAR);
            assert!(std::env::var_os(VAR).is_none());
        }
        assert_eq!(std::env::var(VAR).unwrap(), "outer");
    }

    /// Every `set_var`, `remove_var` and `set_current_dir` in the workspace
    /// goes through this module, so every one of them is under [`lock`] and
    /// restored by a guard. A write anywhere else is a race with every other
    /// test in its binary.
    #[test]
    fn every_env_and_cwd_write_goes_through_process_env() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let this_file = std::fs::canonicalize(file!())
            .or_else(|_| std::fs::canonicalize(crates.join("..").join(file!())))
            .expect("locate this source file");
        let mut offenders = Vec::new();
        let mut stack = vec![crates];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                if path.is_dir() {
                    if name != "target" && name != "node_modules" && !name.starts_with('.') {
                        stack.push(path);
                    }
                    continue;
                }
                if path.extension().is_none_or(|ext| ext != "rs")
                    || std::fs::canonicalize(&path).unwrap() == this_file
                {
                    continue;
                }
                let source = std::fs::read_to_string(&path).unwrap();
                for (n, line) in source.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    if ["env::set_var(", "env::remove_var(", "env::set_current_dir("]
                        .iter()
                        .any(|call| code.contains(call))
                    {
                        offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "change env vars and the working directory through \
             `alms_test_support::process_env` (one lock per test binary, \
             restored on drop), not directly:\n{}",
            offenders.join("\n")
        );
    }
}
