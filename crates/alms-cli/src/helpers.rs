use alms_core::{AgentId, AgentRecord, AlmsConfig};
use alms_session::SqliteStore;

/// Open the SQLite store at the configured DB path.
///
/// Resolves the database path via `AlmsConfig` (which reads `alms.toml` and
/// env vars), ensuring the CLI uses the same database as the gateway.
/// Precedence: `ALMS_DB_PATH` env var > `{data_dir}/alms.db` (where
/// `data_dir` comes from config / `ALMS_DATA_DIR` env var, default `./.alms`).
pub(crate) fn open_db() -> anyhow::Result<SqliteStore> {
    let (store, _) = open_db_with_config()?;
    Ok(store)
}

/// Open DB and return the loaded config (avoids re-parsing alms.toml).
pub(crate) fn open_db_with_config() -> anyhow::Result<(SqliteStore, AlmsConfig)> {
    let config = AlmsConfig::load_or_default();
    let db_path = config.server.db_path();
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok((SqliteStore::open(&db_path)?, config))
}

/// Resolve an agent by UUID or name slug.
pub(crate) fn resolve_agent(store: &SqliteStore, name_or_id: &str) -> anyhow::Result<AgentRecord> {
    if let Ok(uuid) = uuid::Uuid::parse_str(name_or_id) {
        if let Some(agent) = store.load_agent_by_id(AgentId(uuid))? {
            return Ok(agent);
        }
        anyhow::bail!("Agent not found: {name_or_id}");
    }
    match store.load_agent_by_name(name_or_id)? {
        Some(agent) => Ok(agent),
        None => anyhow::bail!("Agent not found: {name_or_id}"),
    }
}

/// Format a chrono DateTime for display.
pub(crate) fn fmt_time(dt: &chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M UTC").to_string()
}

/// Truncate a UUID string to the first 8 characters for display.
pub(crate) fn short_id(id: &impl std::fmt::Display) -> String {
    let s = id.to_string();
    s.get(..8).unwrap_or(&s).to_string()
}

/// Build a full URL from base and path.
pub(crate) fn api_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

/// Build a reqwest client with optional auth token.
pub(crate) fn api_client() -> anyhow::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder();
    if let Ok(token) = std::env::var("ALMS_AUTH_TOKEN") {
        let mut headers = reqwest::header::HeaderMap::new();
        let val =
            reqwest::header::HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| {
                anyhow::anyhow!("ALMS_AUTH_TOKEN contains invalid characters for an HTTP header")
            })?;
        headers.insert(reqwest::header::AUTHORIZATION, val);
        builder = builder.default_headers(headers);
    }
    Ok(builder.build().unwrap_or_else(|_| reqwest::Client::new()))
}

/// Parse an HTTP error response body into a user-friendly message.
///
/// The gateway emits error bodies in **two** shapes and this reads both
/// (#1289, Tim S3 on #1295):
///
/// 1. The house shape from `api_error` — `{"error": {"code", "message"}}`.
/// 2. A **flat** `{"error_code", "message", ...}` built directly by the
///    handler. `POST /runs` alone has three: the DM guard (#1156), the
///    subagent guard (#1289), and the queue-full / shutdown admission
///    errors. Reading only shape 1 meant those printed as a raw JSON dump
///    of the whole body instead of the sentence written for the operator
///    — and for the subagent guard that is the entire audience, since the
///    web UI never offers the path.
///
/// Shape 1 is tried first so a body carrying both is read as the house
/// shape. Anything that parses as neither falls back to the raw body,
/// which is still strictly more than nothing.
pub(crate) fn parse_api_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(msg) = val
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
        {
            return format!("HTTP {status}: {msg}");
        }
        if let Some(msg) = val.get("message").and_then(|m| m.as_str()) {
            return format!("HTTP {status}: {msg}");
        }
    }
    format!("HTTP {status}: {body}")
}

/// Send a GET request and return the response body as JSON Value.
pub(crate) async fn api_get(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
) -> anyhow::Result<serde_json::Value> {
    let url = api_url(base_url, path);
    let resp = client.get(&url).send().await.map_err(|e| {
        if e.is_connect() {
            anyhow::anyhow!(
                "Cannot connect to gateway at {base_url}. Is it running? Start with: alms gateway"
            )
        } else {
            anyhow::anyhow!("Request failed: {e}")
        }
    })?;
    let status = resp.status();
    let body = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("{}", parse_api_error(status, &body));
    }
    Ok(serde_json::from_str(&body)?)
}

/// Send a POST request with JSON body and return the response.
pub(crate) async fn api_post(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
    body: &impl serde::Serialize,
) -> anyhow::Result<(reqwest::StatusCode, serde_json::Value)> {
    let url = api_url(base_url, path);
    let resp = client.post(&url).json(body).send().await.map_err(|e| {
        if e.is_connect() {
            anyhow::anyhow!(
                "Cannot connect to gateway at {base_url}. Is it running? Start with: alms gateway"
            )
        } else {
            anyhow::anyhow!("Request failed: {e}")
        }
    })?;
    let status = resp.status();
    let body_text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("{}", parse_api_error(status, &body_text));
    }
    Ok((status, serde_json::from_str(&body_text)?))
}

/// Send a DELETE request and return the status code.
pub(crate) async fn api_delete(
    client: &reqwest::Client,
    base_url: &str,
    path: &str,
) -> anyhow::Result<reqwest::StatusCode> {
    let url = api_url(base_url, path);
    let resp = client.delete(&url).send().await.map_err(|e| {
        if e.is_connect() {
            anyhow::anyhow!(
                "Cannot connect to gateway at {base_url}. Is it running? Start with: alms gateway"
            )
        } else {
            anyhow::anyhow!("Request failed: {e}")
        }
    })?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await?;
        anyhow::bail!("{}", parse_api_error(status, &body));
    }
    Ok(status)
}

// Test helpers shared across modules
#[cfg(test)]
pub(crate) fn new_store() -> SqliteStore {
    SqliteStore::open_in_memory().unwrap()
}

#[cfg(test)]
pub(crate) fn make_agent(store: &SqliteStore, name: &str) -> AgentRecord {
    let now = chrono::Utc::now();
    let agent = AgentRecord {
        id: AgentId::new(),
        name: name.to_string(),
        description: String::new(),
        model: None,
        posture: None,
        provider: None,
        telegram_token: None,
        thinking_budget_tokens: None,
        reasoning_effort: None,
        gemini_thinking_budget: None,
        summary_provider: None,
        summary_model: None,
        worktree_mode: alms_core::WorktreeMode::Off,
        debug_mode: false,
        is_default: false,
        created_at: now,
        last_active: now,
    };
    store.create_agent(&agent).unwrap();
    agent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_agent_by_name() {
        let store = new_store();
        let agent = make_agent(&store, "atlas");
        let resolved = resolve_agent(&store, "atlas").unwrap();
        assert_eq!(resolved.id, agent.id);
    }

    #[test]
    fn test_resolve_agent_by_uuid() {
        let store = new_store();
        let agent = make_agent(&store, "atlas");
        let resolved = resolve_agent(&store, &agent.id.to_string()).unwrap();
        assert_eq!(resolved.name, "atlas");
    }

    #[test]
    fn test_resolve_agent_not_found() {
        let store = new_store();
        assert!(resolve_agent(&store, "nonexistent").is_err());
    }

    #[test]
    fn test_api_url_basic() {
        assert_eq!(
            api_url("http://localhost:8080", "agents"),
            "http://localhost:8080/agents"
        );
    }

    #[test]
    fn test_api_url_trailing_slash() {
        assert_eq!(
            api_url("http://localhost:8080/", "/agents"),
            "http://localhost:8080/agents"
        );
    }

    #[test]
    fn test_api_url_no_double_slash() {
        assert_eq!(
            api_url("http://localhost:8080/", "agents"),
            "http://localhost:8080/agents"
        );
    }

    #[test]
    fn test_parse_api_error_json() {
        let body = r#"{"error":{"code":"not_found","message":"Agent not found"}}"#;
        let msg = parse_api_error(reqwest::StatusCode::NOT_FOUND, body);
        assert!(msg.contains("Agent not found"));
        assert!(msg.contains("404"));
    }

    /// #1289 / Tim S3: the flat `{"error_code", "message", ...}` shape.
    /// This is the body the subagent guard emits, byte-shaped like the
    /// handler builds it — and `alms run create --session <subagent id>`
    /// is the only client that can reach that guard, so a raw JSON dump
    /// here means the message reaches nobody.
    #[test]
    fn test_parse_api_error_flat_handler_shape() {
        let body = r#"{"error_code":"SUBAGENT_SESSION_NOT_DIRECTLY_RUNNABLE","message":"Subagent sessions are coordinator-driven; turns are triggered via invoke_agent, not POST /runs.","session_id":"11111111-1111-1111-1111-111111111111","context_id":"subagent_22222222-2222-2222-2222-222222222222_reviewer"}"#;
        let msg = parse_api_error(reqwest::StatusCode::BAD_REQUEST, body);

        assert!(msg.contains("400"));
        assert!(
            msg.contains("turns are triggered via invoke_agent"),
            "the operator must get the sentence, not the envelope; got {msg:?}"
        );
        assert!(
            !msg.contains("error_code"),
            "a flat body must not be dumped raw; got {msg:?}"
        );
    }

    /// The house shape wins when a body somehow carries both, so adding
    /// the flat arm cannot change how any existing error renders.
    #[test]
    fn test_parse_api_error_prefers_the_nested_shape() {
        let body =
            r#"{"error":{"code":"NOT_FOUND","message":"nested wins"},"message":"flat loses"}"#;
        let msg = parse_api_error(reqwest::StatusCode::NOT_FOUND, body);
        assert!(msg.contains("nested wins"), "got {msg:?}");
        assert!(!msg.contains("flat loses"), "got {msg:?}");
    }

    /// A JSON body with neither shape still falls back to the raw body
    /// rather than to an empty message.
    #[test]
    fn test_parse_api_error_json_without_either_shape_falls_back_to_the_body() {
        let body = r#"{"unexpected":"payload"}"#;
        let msg = parse_api_error(reqwest::StatusCode::BAD_GATEWAY, body);
        assert!(msg.contains("unexpected"), "got {msg:?}");
    }

    #[test]
    fn test_parse_api_error_plain_text() {
        let msg = parse_api_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "boom");
        assert!(msg.contains("500"));
        assert!(msg.contains("boom"));
    }

    #[test]
    fn test_short_id_normal_uuid() {
        let id = uuid::Uuid::new_v4();
        let s = short_id(&id);
        assert_eq!(s.len(), 8);
    }

    #[test]
    fn test_short_id_short_string() {
        let s = short_id(&"abc");
        assert_eq!(s, "abc");
    }
}
