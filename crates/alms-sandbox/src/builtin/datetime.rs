// SPDX-License-Identifier: Apache-2.0

use crate::Tool;
use crate::error::SandboxResult;
use chrono::{Local, Utc};
use serde_json::Value;

/// Datetime tool - returns the current date and time.
///
/// Agents use this to know what time it is. Returns ISO 8601 timestamp,
/// human-readable format, and UTC offset. Always auto-approved.
#[derive(Debug, Clone, Default)]
pub struct DatetimeTool;

impl DatetimeTool {
    /// Create a new datetime tool
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Tool for DatetimeTool {
    fn name(&self) -> &str {
        "datetime"
    }

    fn description(&self) -> &str {
        "Returns the current date and time in both UTC and local device timezone. \
         Includes ISO 8601 format, human-readable format, timezone name, and UTC offset. \
         Use this whenever you need to know the current time."
    }

    fn is_builtin(&self) -> bool {
        true
    }

    fn is_auto_approved(&self) -> bool {
        true
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
        })
    }

    async fn execute(&self, _params: Value) -> SandboxResult<Value> {
        let utc_now = Utc::now();
        let local_now = Local::now();
        let utc_offset = local_now.format("%:z").to_string();
        let local_timezone = iana_time_zone::get_timezone().unwrap_or_else(|_| "Unknown".into());
        Ok(serde_json::json!({
            "iso": utc_now.to_rfc3339(),
            "human": utc_now.format("%A, %B %-d, %Y %-I:%M %p").to_string(),
            "timezone": "UTC",
            "local_iso": local_now.to_rfc3339(),
            "local_human": local_now.format("%A, %B %-d, %Y %-I:%M %p").to_string(),
            "local_timezone": local_timezone,
            "utc_offset": utc_offset,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_datetime_tool_returns_valid_fields() {
        let tool = DatetimeTool::new();
        let result = tool.execute(serde_json::json!({})).await.unwrap();

        // Must contain all expected UTC fields
        assert!(result.get("iso").is_some(), "missing 'iso' field");
        assert!(result.get("human").is_some(), "missing 'human' field");
        assert_eq!(result["timezone"], "UTC");

        // Must contain all expected local fields
        assert!(
            result.get("local_iso").is_some(),
            "missing 'local_iso' field"
        );
        assert!(
            result.get("local_human").is_some(),
            "missing 'local_human' field"
        );
        assert!(
            result.get("utc_offset").is_some(),
            "missing 'utc_offset' field"
        );

        // local_timezone should be an IANA name (e.g. "Europe/Istanbul"), not a numeric offset
        let tz = result["local_timezone"]
            .as_str()
            .expect("local_timezone must be a string");
        assert!(!tz.is_empty(), "local_timezone must not be empty");
        assert!(
            tz.contains('/') || tz == "Unknown",
            "local_timezone should be an IANA name like 'Region/City', got: {tz}"
        );

        // utc_offset should look like a numeric offset (e.g. "+03:00")
        let offset = result["utc_offset"]
            .as_str()
            .expect("utc_offset must be a string");
        assert!(
            offset.starts_with('+') || offset.starts_with('-'),
            "utc_offset should start with +/-, got: {offset}"
        );

        // UTC ISO string must parse back into a valid DateTime
        let iso_str = result["iso"].as_str().unwrap();
        chrono::DateTime::parse_from_rfc3339(iso_str)
            .unwrap_or_else(|_| panic!("invalid ISO 8601: {}", iso_str));

        // Local ISO string must also parse back into a valid DateTime
        let local_iso_str = result["local_iso"].as_str().unwrap();
        chrono::DateTime::parse_from_rfc3339(local_iso_str)
            .unwrap_or_else(|_| panic!("invalid local ISO 8601: {}", local_iso_str));
    }

    #[test]
    fn test_datetime_tool_is_auto_approved() {
        let tool = DatetimeTool::new();
        assert!(tool.is_auto_approved());
        assert!(tool.is_builtin());
    }
}
