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
//! lock (declared first, so dropped last) is released.
//!
//! # Why taking it twice panics
//!
//! One lock means a test that already holds it and then takes it again, for
//! instance through a helper guard that takes it itself, would wait on itself
//! forever: a hung test binary rather than a failure. [`lock`] tracks the
//! holder per thread and panics on re-entry instead, naming the mistake.
//! A test body stays on its thread (`#[tokio::test]` runs it with
//! `block_on`, on either runtime flavour), so the per-thread record is the
//! test's.
//!
//! # Rules
//!
//! * Take [`lock`] first, then the guards, as locals in that order. The
//!   guards check that this thread holds it.
//! * Change env vars and the working directory only through this module.
//!   `clippy.toml` at the workspace root disallows `std::env::set_var`,
//!   `remove_var` and `set_current_dir` everywhere else, and CI runs clippy
//!   over test code with `-D warnings`.
//! * A test that **reads** state another test changes, e.g. one that
//!   compares a path against `std::env::current_dir()`, takes [`lock`] too.
//!   Prefer not reading or writing process state at all where the code
//!   under test can be handed the value instead.

use std::cell::Cell;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

static LOCK: Mutex<()> = Mutex::new(());

thread_local! {
    static HELD_BY_THIS_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Exclusive access to this test binary's environment and working directory.
/// Released on drop.
#[must_use = "the lock is released when this is dropped"]
pub struct EnvLock {
    _guard: MutexGuard<'static, ()>,
}

impl Drop for EnvLock {
    fn drop(&mut self) {
        // Runs before `_guard` is released, so no other thread can take the
        // lock while this one still reads as holding it.
        HELD_BY_THIS_THREAD.with(|held| held.set(false));
    }
}

/// Take this binary's env and working-directory lock, recovered rather than
/// propagated if a previous holder panicked (see the module docs for why that
/// is sound).
///
/// # Panics
///
/// If this thread already holds it: waiting would never end.
pub fn lock() -> EnvLock {
    if HELD_BY_THIS_THREAD.with(Cell::get) {
        panic!(
            "process_env::lock() is already held by this test; it is not \
             re-entrant. Guards that take it themselves (EnvGuard, \
             BudgetValidationEnvGuard) must not be combined with a lock the \
             test already holds"
        );
    }
    let guard = LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    HELD_BY_THIS_THREAD.with(|held| held.set(true));
    EnvLock { _guard: guard }
}

fn assert_locked(what: &str) {
    assert!(
        HELD_BY_THIS_THREAD.with(Cell::get),
        "{what} needs process_env::lock() held first: another test could read \
         or change the same state meanwhile"
    );
}

// The four sanctioned writers of process state. `clippy.toml` disallows these
// calls everywhere else in the workspace.

#[expect(
    clippy::disallowed_methods,
    reason = "the one sanctioned env writer, under `lock()`"
)]
fn set_var(key: &OsStr, value: &OsStr) {
    // SAFETY: env writes race with reads on other threads. Every test that
    // writes or reads shared env state holds `lock()` (module rules), and
    // clippy keeps writes from happening anywhere else.
    unsafe { std::env::set_var(key, value) }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the one sanctioned env writer, under `lock()`"
)]
fn remove_var(key: &OsStr) {
    // SAFETY: as for `set_var`.
    unsafe { std::env::remove_var(key) }
}

/// Sets or removes one env var and puts its previous value back on drop,
/// including when the test panics. Needs [`lock`] held.
#[must_use = "the variable is restored when the guard is dropped"]
pub struct VarGuard {
    key: OsString,
    original: Option<OsString>,
}

impl VarGuard {
    pub fn set(key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        let guard = Self::capture(key.as_ref());
        set_var(key.as_ref(), value.as_ref());
        guard
    }

    pub fn remove(key: impl AsRef<OsStr>) -> Self {
        let guard = Self::capture(key.as_ref());
        remove_var(key.as_ref());
        guard
    }

    fn capture(key: &OsStr) -> Self {
        assert_locked("VarGuard");
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
/// when the test panics. Needs [`lock`] held.
#[must_use = "the working directory is restored when the guard is dropped"]
pub struct CurrentDirGuard {
    original: PathBuf,
}

impl CurrentDirGuard {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one sanctioned working-directory writer, under `lock()`"
    )]
    pub fn set(dir: impl AsRef<Path>) -> Self {
        assert_locked("CurrentDirGuard");
        let original = std::env::current_dir().expect("the working directory is readable");
        std::env::set_current_dir(dir.as_ref()).expect("change the working directory");
        Self { original }
    }
}

impl Drop for CurrentDirGuard {
    #[expect(
        clippy::disallowed_methods,
        reason = "the one sanctioned working-directory writer, under `lock()`"
    )]
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

    /// Taking the lock twice on one thread would wait on itself forever. It
    /// must fail at once, naming the cause, and the panic must leave the lock
    /// usable for everyone else.
    #[test]
    fn taking_the_lock_twice_panics_instead_of_hanging() {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            // Reports back even when the body panics, so a hang is the only
            // way not to hear from it.
            struct Report(std::sync::mpsc::Sender<()>);
            impl Drop for Report {
                fn drop(&mut self) {
                    let _ = self.0.send(());
                }
            }
            let _report = Report(done_tx);
            let _first = lock();
            let _second = lock();
        });
        done_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("taking the lock a second time hung instead of panicking");
        let payload = handle.join().expect_err("re-entry must panic");
        let message = payload
            .downcast_ref::<&str>()
            .map(|m| m.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        assert!(message.contains("already held by this test"), "{message}");

        let _env = lock();
        assert!(HELD_BY_THIS_THREAD.with(Cell::get));
    }

    /// A guard taken without the lock is the race this module exists to stop.
    #[test]
    fn a_guard_without_the_lock_panics() {
        const UNLOCKED: &str = "ALMS_TEST_SUPPORT_PROCESS_ENV_UNLOCKED_PROBE";
        let joined = std::thread::spawn(|| {
            let _var = VarGuard::set(UNLOCKED, "unlocked");
        })
        .join();
        assert!(joined.is_err());
        let _env = lock();
        assert_eq!(std::env::var_os(UNLOCKED), None, "nothing was written");
    }
}
