// SPDX-License-Identifier: Apache-2.0

//! Interest-cache-safe `tracing` capture harness for log-asserting tests.
//!
//! Events are captured **structured** — level, target, message and the
//! recorded fields — not as rendered text. The fields are the contract a
//! log-asserting test pins; the rendering belongs to whichever formatter
//! the deployment installs, and it has changed once already (the old
//! string harness left tests hedging `direction="off->git"` against
//! `direction=off->git`, which is the tell that the rendering was never
//! the thing being asserted).
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
//! binary, and capturing becomes a per-thread concern rather than a
//! subscriber-level one:
//!
//! * `CaptureLayer::register_callsite` returns [`Interest::sometimes`]
//!   for every callsite — never [`Interest::never`] — so no cached
//!   interest can short-circuit an event, whichever thread touches a
//!   callsite first. There is no thread without a subscriber any more.
//! * `CaptureLayer::max_level_hint` pins `tracing`'s process-global
//!   `MAX_LEVEL` to `TRACE`, so the macros' static level check cannot
//!   short-circuit either.
//! * The actual "should this be recorded" decision moves to
//!   `CaptureLayer::enabled`, which is re-evaluated per event **on the
//!   emitting thread** against that thread's capture slot. Threads that
//!   are not capturing answer `false`.
//!
//! Because no per-capture `Dispatch` is created any more, the interest
//! cache is never re-evaluated mid-run, and the capture slot is
//! thread-local, so parallel tests cannot see each other's events.
//!
//! # What this costs the tests that do not capture
//!
//! Pinning interest to `sometimes` and `MAX_LEVEL` to `TRACE` gives up
//! `tracing`'s two static short-circuits, so every callsite in a test
//! binary that calls [`capture_events`] — including the `debug!`/`trace!`
//! ones that used to die at the level check — now reaches a **runtime
//! dispatch**: a `MAX_LEVEL` load, an `Interest` load, a global-dispatch
//! lookup and one `Layered::enabled` call before `CaptureLayer::enabled`
//! reads the thread-local slot and answers `false`. That is tens of
//! nanoseconds and allocation-free — `event!` keeps the field
//! expressions inside its `if enabled` branch, so nothing is formatted.
//! Cheap, but a dispatch rather than a thread-local read.
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
use std::collections::BTreeMap;
use std::fmt;
use std::ops::Deref;
use std::sync::{Arc, Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

/// One `tracing` event as the subscriber saw it: severity, target, the
/// message, and every other recorded field rendered to a string.
///
/// Field values are rendered the way the macro recorded them — a `&str`
/// or a `%value` lands as its bare text (no quotes), a `?value` as its
/// `Debug` form, numbers and booleans via `to_string`. Assert on
/// [`field`](Self::field), not on a rendering of the whole line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedEvent {
    pub level: Level,
    pub target: String,
    pub message: String,
    pub fields: BTreeMap<String, String>,
}

impl CapturedEvent {
    /// The recorded value of `name`, or `None` when the event did not
    /// carry that field.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    /// Whether the event carried a field called `name`, whatever its value.
    pub fn has_field(&self, name: &str) -> bool {
        self.fields.contains_key(name)
    }
}

impl fmt::Display for CapturedEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}: {}", self.level, self.target, self.message)?;
        for (k, v) in &self.fields {
            write!(f, " {k}={v:?}")?;
        }
        Ok(())
    }
}

/// The events one capture window recorded, in emission order.
///
/// Derefs to `[CapturedEvent]`, so the slice and iterator vocabulary
/// (`len`, `iter().any(..)`, `iter().filter(..)`) is the assertion API.
/// `Display` renders one event per line for panic messages.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapturedEvents(pub Vec<CapturedEvent>);

impl CapturedEvents {
    /// The events emitted at `target`, in order.
    pub fn at_target<'a>(&'a self, target: &'a str) -> impl Iterator<Item = &'a CapturedEvent> {
        self.iter().filter(move |e| e.target == target)
    }
}

impl Deref for CapturedEvents {
    type Target = [CapturedEvent];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for CapturedEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return write!(f, "(no events captured)");
        }
        for e in &self.0 {
            writeln!(f, "{e}")?;
        }
        Ok(())
    }
}

/// One thread's in-flight capture: the minimum severity it wants and
/// the vector the recorded events land in.
#[derive(Clone)]
struct Capture {
    level: Level,
    events: Arc<Mutex<Vec<CapturedEvent>>>,
}

thread_local! {
    /// The calling thread's active capture, or `None` when this thread
    /// is not inside [`capture_events`].
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

/// Renders an event's fields into a [`CapturedEvent`].
struct FieldRecorder {
    message: String,
    fields: BTreeMap<String, String>,
}

impl FieldRecorder {
    fn put(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = value;
        } else {
            self.fields.insert(field.name().to_string(), value);
        }
    }
}

impl Visit for FieldRecorder {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        // `%value` arrives here wrapped in a type whose `Debug` is the
        // value's `Display`, so this is the bare text for those; a
        // `?value` is its `Debug` form; a format-args message is the
        // formatted string.
        self.put(field, format!("{value:?}"));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field, value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field, value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field, value.to_string());
    }

    fn record_i128(&mut self, field: &Field, value: i128) {
        self.put(field, value.to_string());
    }

    fn record_u128(&mut self, field: &Field, value: u128) {
        self.put(field, value.to_string());
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field, value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field, value.to_string());
    }

    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field, value.to_string());
    }
}

/// The layer that keeps every callsite permanently live (see the module
/// docs), gates recording on the emitting thread's capture slot, and
/// records the events that pass.
struct CaptureLayer;

impl<S: Subscriber> Layer<S> for CaptureLayer {
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

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        // `enabled` has already vetted the event, so a missing slot here
        // can only happen if some other layer re-enables an event on a
        // non-capturing thread — in which case the event is dropped.
        let Some(sink) =
            ACTIVE.with_borrow(|active| active.as_ref().map(|c| Arc::clone(&c.events)))
        else {
            return;
        };
        let mut recorder = FieldRecorder {
            message: String::new(),
            fields: BTreeMap::new(),
        };
        event.record(&mut recorder);
        let meta = event.metadata();
        sink.lock().unwrap().push(CapturedEvent {
            level: *meta.level(),
            target: meta.target().to_string(),
            message: recorder.message,
            fields: recorder.fields,
        });
    }
}

/// Install the process-wide capture subscriber. Idempotent; the first
/// [`capture_events`] call in the binary wins.
fn install_global_subscriber() {
    static INSTALLED: OnceLock<()> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        let subscriber = tracing_subscriber::registry().with(CaptureLayer);
        tracing::subscriber::set_global_default(subscriber).expect(
            "a test binary that uses capture_events must not install any other global tracing subscriber",
        );
    });
}

/// Run `f` with every `tracing` event at `level` or more severe emitted
/// **on this thread** recorded, and return the recorded events.
///
/// Unlike `tracing::subscriber::with_default`, this is immune to
/// `tracing`'s global callsite-interest cache — see the module docs and
/// #1221.
pub fn capture_events<F: FnOnce()>(level: Level, f: F) -> CapturedEvents {
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

    let events = Arc::new(Mutex::new(Vec::new()));
    {
        let _guard = ActiveGuard::install(Capture {
            level,
            events: Arc::clone(&events),
        });
        f();
    }
    let recorded = std::mem::take(&mut *events.lock().unwrap());
    CapturedEvents(recorded)
}

#[cfg(test)]
mod tests {
    use super::capture_events;

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
    /// the capture comes back empty, which is exactly the empty value in
    /// the #1221 CI panics.
    ///
    /// To watch it fail, restore the old `with_default` body of
    /// `capture_events` and run this test with `--test-threads=1` (or
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

        let captured = capture_events(tracing::Level::WARN, || {
            std::thread::spawn(emit_probe)
                .join()
                .expect("probe thread must not panic");
            emit_probe();
        });

        assert!(
            captured
                .iter()
                .any(|e| e.field("probe") == Some("callsite-interest")),
            "capture_events must capture an event whose callsite was first \
             registered on a thread with no subscriber installed; got {captured}"
        );
    }

    /// The capture slot is per-thread: an event emitted on another
    /// thread must not leak into this thread's capture. (The old harness
    /// got this from `with_default` being thread-scoped; the replacement
    /// has to keep it.)
    #[test]
    fn does_not_capture_events_from_other_threads() {
        let captured = capture_events(tracing::Level::WARN, || {
            std::thread::spawn(|| {
                tracing::warn!(target: "alms.test_log_capture", "emitted off-thread");
            })
            .join()
            .expect("probe thread must not panic");
        });

        assert!(
            captured.is_empty(),
            "a capture must only see events emitted on its own thread; got {captured}"
        );
    }

    /// The severity gate moved from the subscriber to the per-capture
    /// slot, so pin it: a WARN capture must record WARN and ERROR and
    /// drop INFO.
    #[test]
    fn honours_the_requested_level() {
        let captured = capture_events(tracing::Level::WARN, || {
            tracing::info!(target: "alms.test_log_capture", "info line");
            tracing::warn!(target: "alms.test_log_capture", "warn line");
            tracing::error!(target: "alms.test_log_capture", "error line");
        });

        let levels: Vec<tracing::Level> = captured.iter().map(|e| e.level).collect();
        assert_eq!(
            levels,
            vec![tracing::Level::WARN, tracing::Level::ERROR],
            "got {captured}"
        );
    }

    /// Events outside a capture window are not recorded — neither the
    /// ones before it nor the ones after — and the level, target, message
    /// and fields arrive as the macro recorded them.
    #[test]
    fn records_level_target_message_and_fields_inside_the_window_only() {
        tracing::warn!(target: "alms.test_log_capture", "before the window");
        let captured = capture_events(tracing::Level::INFO, || {
            let name = "atlas";
            let direction = "off->git";
            tracing::warn!(
                target: "alms.test_log_capture",
                agent_name = %name,
                direction,
                count = 3u64,
                flag = true,
                detail = ?Some("x"),
                "flip {} for {}",
                direction,
                name
            );
        });
        tracing::warn!(target: "alms.test_log_capture", "after the window");

        assert_eq!(captured.len(), 1, "got {captured}");
        let e = &captured[0];
        assert_eq!(e.level, tracing::Level::WARN);
        assert_eq!(e.target, "alms.test_log_capture");
        assert_eq!(e.message, "flip off->git for atlas");
        // `%` and a bare `&str` both land as the bare text — the two
        // renderings the old string harness had to hedge between.
        assert_eq!(e.field("agent_name"), Some("atlas"));
        assert_eq!(e.field("direction"), Some("off->git"));
        assert_eq!(e.field("count"), Some("3"));
        assert_eq!(e.field("flag"), Some("true"));
        assert_eq!(e.field("detail"), Some("Some(\"x\")"));
        assert!(!e.has_field("absent"));
    }

    /// The static `MAX_LEVEL` short-circuit is disabled for the whole
    /// binary once the harness is installed, so a `debug!` inside a
    /// capture reaches the subscriber.
    #[test]
    fn pins_the_global_max_level_to_trace() {
        let captured = capture_events(tracing::Level::TRACE, || {
            tracing::trace!(target: "alms.test_log_capture", "trace line");
        });
        assert_eq!(
            tracing::level_filters::LevelFilter::current(),
            tracing::level_filters::LevelFilter::TRACE
        );
        assert_eq!(captured.len(), 1, "got {captured}");
        assert_eq!(captured[0].level, tracing::Level::TRACE);
    }
}
