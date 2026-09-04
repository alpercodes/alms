// SPDX-License-Identifier: Apache-2.0

//! Interest-cache-safe `tracing` capture harness for log-asserting tests.
//!
//! # The defect this exists to prevent (#1221)
//!
//! The obvious way to assert on a structured log is to wrap the code
//! under test in [`tracing::subscriber::with_default`] and read back an
//! in-memory buffer. That is what every log-asserting test in the
//! workspace used to do, and it is subtly, intermittently wrong.
//!
//! `with_default` installs a subscriber on **one thread for the
//! duration of one closure**, but `tracing` caches each callsite's
//! [`Interest`] in a **process-global** static. That cache is only
//! recomputed when a new `Dispatch` is created, and when at most one
//! dispatcher is registered `tracing_core` takes a fast path that
//! evaluates a brand-new callsite against *the registering thread's*
//! current subscriber. Under `cargo test`'s parallel harness that
//! produces this interleaving:
//!
//! 1. test A calls `with_default`; a `Dispatch` is created, which
//!    re-evaluates every callsite registered **so far**;
//! 2. test B, on another thread with **no** subscriber installed,
//!    reaches the callsite A is about to assert on for the first time.
//!    It is registered *now* and evaluated against B's absent
//!    subscriber, so `Interest::never()` is cached process-wide;
//! 3. test A emits the event — the cached `never` short-circuits the
//!    macro before the event ever reaches A's subscriber.
//!
//! Test A then asserts against an **empty** buffer. That is #1221:
//! order-dependent, invisible when the test is run in isolation, and it
//! presents as an unrelated assertion failure in whichever test
//! happened to lose the race.
//!
//! # How this module removes the failure mode
//!
//! One subscriber is installed as the **global** default, once per test
//! binary, and capturing becomes a *writer*-level concern rather than a
//! subscriber-level one:
//!
//! * `CaptureFilter::register_callsite` returns [`Interest::sometimes`]
//!   for every callsite — never [`Interest::never`] — so no cached
//!   interest can short-circuit an event, whichever thread touches a
//!   callsite first. There is no thread without a subscriber any more.
//! * `CaptureFilter::max_level_hint` pins `tracing`'s process-global
//!   `MAX_LEVEL` to `TRACE`, so the macros' static level check cannot
//!   short-circuit either.
//! * The actual "should this be recorded" decision moves to
//!   `CaptureFilter::enabled`, which is re-evaluated per event **on the
//!   emitting thread** against that thread's capture slot. Threads that
//!   are not capturing answer `false`.
//!
//! Because no per-capture `Dispatch` is created any more, the interest
//! cache is never re-evaluated mid-run, and the capture buffer is
//! thread-local, so parallel tests cannot see each other's output.
//!
//! # What this costs the tests that do not capture
//!
//! Pinning interest to `sometimes` and `MAX_LEVEL` to `TRACE` gives up
//! `tracing`'s two static short-circuits, so every callsite in a test
//! binary that calls [`capture_logs`] — including the `debug!`/`trace!`
//! ones that used to die at the level check — now reaches a **runtime
//! dispatch**: a `MAX_LEVEL` load, an `Interest` load, a global-dispatch
//! lookup and two `Layered::enabled` calls before
//! `CaptureFilter::enabled` reads the thread-local slot and answers
//! `false`. That is tens of nanoseconds and allocation-free —
//! `CaptureFilter` is the **outer** layer, so its `false` short-circuits
//! before the `fmt` layer or the registry are touched, and `event!`
//! keeps the field expressions inside its `if enabled` branch, so
//! nothing is formatted. Cheap, but a dispatch rather than a
//! thread-local read.
//!
//! **It stays cheap only because `tokio_unstable` is off.** The
//! workspace enables tokio's `tracing` feature (root `Cargo.toml`), but
//! tokio gates its per-task-poll and per-resource instrumentation behind
//! `cfg_trace!` = `#[cfg(all(tokio_unstable, feature = "tracing"))]`,
//! and this repo sets that cfg nowhere — no `.cargo/config.toml`, no
//! `RUSTFLAGS`. Turning `tokio_unstable` on (`console-subscriber` is the
//! usual reason) would silently make this harness resurrect tokio's
//! per-poll instrumentation across every test in the binary. If test
//! wall-time jumps after a build-flag change, start here.
//!
//! # One crate, one copy
//!
//! This used to be mirrored by hand between `alms-core` and
//! `alms-gateway` because each test binary needs its own global
//! subscriber. That is still true, and a shared crate still provides it:
//! every test target links its own copy of this module's statics.
//!
//! [`Interest`]: tracing::subscriber::Interest
//! [`Interest::never`]: tracing::subscriber::Interest::never
//! [`Interest::sometimes`]: tracing::subscriber::Interest::sometimes

use std::cell::RefCell;
use std::io;
use std::sync::{Arc, Mutex, OnceLock};

use tracing::level_filters::LevelFilter;
use tracing::subscriber::Interest;
use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

/// One thread's in-flight capture: the minimum severity it wants and
/// the buffer the formatted events land in.
#[derive(Clone)]
struct Capture {
    level: Level,
    buf: Arc<Mutex<Vec<u8>>>,
}

thread_local! {
    /// The calling thread's active capture, or `None` when this thread
    /// is not inside [`capture_logs`].
    static ACTIVE: RefCell<Option<Capture>> = const { RefCell::new(None) };
}

/// Restores the previous capture on drop, so a panicking assertion
/// inside a capture closure can never leave the slot set for whatever
/// libtest runs on this thread next.
struct ActiveGuard(Option<Capture>);

impl ActiveGuard {
    fn install(capture: Capture) -> Self {
        Self(ACTIVE.with_borrow_mut(|slot| slot.replace(capture)))
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        ACTIVE.with_borrow_mut(|slot| *slot = self.0.take());
    }
}

/// The layer that keeps every callsite permanently live (see the module
/// docs) while gating the actual recording on the emitting thread's
/// capture slot.
struct CaptureFilter;

impl<S: Subscriber> Layer<S> for CaptureFilter {
    fn register_callsite(&self, _meta: &'static Metadata<'static>) -> Interest {
        // Deliberately never `Interest::never()`: a cached `never` is
        // the #1221 defect. `sometimes` keeps the callsite live and
        // defers to `enabled` below, which is re-evaluated per event.
        Interest::sometimes()
    }

    fn enabled(&self, meta: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        ACTIVE.with_borrow(|active| {
            active
                .as_ref()
                .is_some_and(|capture| *meta.level() <= capture.level)
        })
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::TRACE)
    }
}

/// Routes each recorded event into the emitting thread's capture
/// buffer. `CaptureFilter::enabled` has already vetted the event, so a
/// missing slot here can only happen if some other layer re-enables an
/// event on a non-capturing thread — in which case the bytes are
/// dropped.
struct CaptureWriter(Option<Arc<Mutex<Vec<u8>>>>);

impl io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(sink) = &self.0 {
            sink.lock().unwrap().extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct ThreadLocalCapture;

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadLocalCapture {
    type Writer = CaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CaptureWriter(ACTIVE.with_borrow(|active| active.as_ref().map(|c| Arc::clone(&c.buf))))
    }
}

/// Install the process-wide capture subscriber. Idempotent; the first
/// [`capture_logs`] call in the binary wins.
fn install_global_subscriber() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let subscriber = tracing_subscriber::registry()
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(ThreadLocalCapture)
                    // `with_target(true)` is required so assertions can
                    // match on targets such as `alms.worktree`.
                    .with_target(true)
                    .without_time()
                    .with_ansi(false),
            )
            .with(CaptureFilter);
        tracing::subscriber::set_global_default(subscriber).expect(
            "a test binary that uses capture_logs must not install any other global tracing subscriber",
        );
    });
}

/// Run `f` with every `tracing` event at `level` or more severe emitted
/// **on this thread** captured into an in-memory buffer, and return the
/// captured text.
///
/// Unlike `tracing::subscriber::with_default`, this is immune to
/// `tracing`'s global callsite-interest cache — see the module docs and
/// #1221.
pub fn capture_logs<F: FnOnce()>(level: Level, f: F) -> String {
    install_global_subscriber();

    // Installing the global default above already re-evaluates every
    // callsite registered before it existed. This second rebuild closes
    // the remaining window: a callsite registered on a subscriber-less
    // thread *concurrently with* that install could still have cached
    // `Interest::never()`, and nothing else would ever recompute it,
    // because we only ever create one `Dispatch`. Walking the callsite
    // registry is cheap and this runs a handful of times per binary.
    //
    // `tracing::callsite` is a `#[doc(hidden)]` re-export of the
    // `tracing_core::callsite` module; `rebuild_interest_cache` is
    // public, documented API there.
    tracing::callsite::rebuild_interest_cache();

    let buf = Arc::new(Mutex::new(Vec::new()));
    {
        let _guard = ActiveGuard::install(Capture {
            level,
            buf: Arc::clone(&buf),
        });
        f();
    }
    let bytes = buf.lock().unwrap();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[cfg(test)]
mod tests {
    use super::capture_logs;

    /// #1221 regression guard, and the deterministic reproduction of the
    /// flake this module exists to kill.
    ///
    /// The interleaving below is the one that fires in CI, made
    /// deterministic: the capture is already in flight when another
    /// thread — with no subscriber of its own — touches the callsite for
    /// the very first time. Against the old `with_default` harness the
    /// brand-new callsite is evaluated against *that* thread's absent
    /// subscriber, `Interest::never()` is cached process-wide, and the
    /// capturing thread's own event is dropped before it is dispatched:
    /// `captured` comes back `""`, which is exactly the empty value in
    /// the #1221 CI panics.
    ///
    /// To watch it fail, restore the old `with_default` body of
    /// `capture_logs` and run this test with `--test-threads=1` (or
    /// `--exact`). The isolation matters: `tracing_core` only takes the
    /// single-dispatcher fast path that consults *the registering
    /// thread's* subscriber while at most one `Dispatch` is registered,
    /// so two overlapping `with_default` captures accidentally shield
    /// each other's callsites. That is precisely why #1221 was
    /// intermittent instead of constant.
    #[test]
    fn captures_a_callsite_first_registered_on_a_subscriberless_thread() {
        // A callsite that appears nowhere else in the tree, so it is
        // guaranteed to be unregistered when this test starts —
        // otherwise an earlier test would have registered it and there
        // would be nothing left to race on.
        fn emit_probe() {
            tracing::warn!(
                target: "alms.test_log_capture",
                probe = "callsite-interest",
                "interest-poisoning probe"
            );
        }

        let captured = capture_logs(tracing::Level::WARN, || {
            std::thread::spawn(emit_probe)
                .join()
                .expect("probe thread must not panic");
            emit_probe();
        });

        assert!(
            captured.contains("interest-poisoning probe"),
            "capture_logs must capture an event whose callsite was first \
             registered on a thread with no subscriber installed; got {captured:?}"
        );
    }

    /// The capture buffer is per-thread: an event emitted on another
    /// thread must not leak into this thread's capture. (The old harness
    /// got this from `with_default` being thread-scoped; the replacement
    /// has to keep it.)
    #[test]
    fn does_not_capture_events_from_other_threads() {
        let captured = capture_logs(tracing::Level::WARN, || {
            std::thread::spawn(|| {
                tracing::warn!(target: "alms.test_log_capture", "emitted off-thread");
            })
            .join()
            .expect("probe thread must not panic");
        });

        assert!(
            !captured.contains("emitted off-thread"),
            "a capture must only see events emitted on its own thread; got {captured:?}"
        );
    }

    /// The severity gate moved from the subscriber to the per-capture
    /// slot, so pin it: a WARN capture must record WARN and ERROR and
    /// drop INFO.
    #[test]
    fn honours_the_requested_level() {
        let captured = capture_logs(tracing::Level::WARN, || {
            tracing::info!(target: "alms.test_log_capture", "info line");
            tracing::warn!(target: "alms.test_log_capture", "warn line");
            tracing::error!(target: "alms.test_log_capture", "error line");
        });

        assert!(
            !captured.contains("info line"),
            "INFO is below a WARN capture and must be dropped; got {captured:?}"
        );
        assert!(
            captured.contains("warn line") && captured.contains("error line"),
            "WARN and ERROR must both be captured at WARN; got {captured:?}"
        );
    }

    /// Events emitted outside any capture window must not accumulate
    /// anywhere — the global subscriber is always installed, so this is
    /// worth pinning.
    #[test]
    fn captures_nothing_outside_a_capture_window() {
        tracing::warn!(target: "alms.test_log_capture", "outside the window");
        let captured = capture_logs(tracing::Level::WARN, || {});

        assert!(
            captured.is_empty(),
            "a capture must start empty and stay empty when nothing is \
             emitted inside it; got {captured:?}"
        );
    }

    /// The `max_level_hint` pin is what stops the macros' static level
    /// check from short-circuiting before an event is ever dispatched.
    ///
    /// Unlike the end-to-end guard above, this one fails
    /// **deterministically** if the harness is ever restored to
    /// `with_default`: that design's `MAX_LEVEL` is the max hint across
    /// the live dispatchers, i.e. whatever level the capture itself asked
    /// for, and no call site in the tree captures below `INFO` — so it
    /// can never be `TRACE`.
    #[test]
    fn pins_the_global_max_level_to_trace() {
        capture_logs(tracing::Level::WARN, || {
            assert_eq!(
                tracing::level_filters::LevelFilter::current(),
                tracing::level_filters::LevelFilter::TRACE,
                "the capture subscriber must pin the global MAX_LEVEL to \
                 TRACE, or the macros' static level check will drop events \
                 before they reach the subscriber"
            );
        });
    }
}
