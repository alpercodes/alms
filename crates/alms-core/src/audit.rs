// SPDX-License-Identifier: Apache-2.0

use crate::{RunId, SessionId, Timestamp};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditDecision {
    Allow,
    Deny,
    /// A runtime failure during tool execution (as opposed to a policy denial).
    /// Distinguishes "we chose not to run this" (`Deny`) from "we ran it and it
    /// failed" (`Error`).
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    pub session_id: SessionId,
    pub run_id: Option<RunId>,
    pub tool: String,
    pub decision: AuditDecision,
    pub params: serde_json::Value,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub timestamp: Timestamp,
}

impl AuditEvent {
    pub fn allow(
        session_id: SessionId,
        tool: impl Into<String>,
        params: serde_json::Value,
        result: serde_json::Value,
    ) -> Self {
        Self {
            session_id,
            run_id: None,
            tool: tool.into(),
            decision: AuditDecision::Allow,
            params,
            result: Some(result),
            error: None,
            timestamp: Timestamp::now(),
        }
    }

    pub fn deny(
        session_id: SessionId,
        tool: impl Into<String>,
        params: serde_json::Value,
        error: impl Into<String>,
    ) -> Self {
        Self {
            session_id,
            run_id: None,
            tool: tool.into(),
            decision: AuditDecision::Deny,
            params,
            result: None,
            error: Some(error.into()),
            timestamp: Timestamp::now(),
        }
    }
}
