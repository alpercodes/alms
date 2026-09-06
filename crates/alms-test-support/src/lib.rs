// SPDX-License-Identifier: Apache-2.0

//! Dev-only fixtures shared by the workspace's test targets.
//!
//! Everything here used to exist as per-crate copies that had to be kept
//! in step by hand (`test_log_capture.rs` in `alms-core` and
//! `alms-gateway`, `read_full_http_request` in three test modules,
//! `init_git_repo` in two). One copy, one place to fix.
//!
//! # Rules
//!
//! * Only ever a `[dev-dependencies]` entry. Nothing in here ships.
//! * No `alms-*` dependencies. `alms-core`'s own unit tests use this
//!   crate; a dev-dependency that depends on the crate under test is
//!   compiled against a *second* build of that crate, so its types are
//!   not `crate::*`'s types inside the test binary. Fixtures that need
//!   `alms-core` types live in `alms-core` behind its `test-support`
//!   feature instead (see `AgentRecord::for_test`).
//! * Per-process state (the global `tracing` subscriber installed by
//!   [`capture_logs`]) is per test binary: each crate's test target links
//!   its own copy of this crate's statics, which is exactly the isolation
//!   the old hand-mirrored copies were providing.

pub mod git;
pub mod http;
pub mod log_capture;

pub use git::init_git_repo;
pub use http::read_full_http_request;
pub use log_capture::capture_logs;
