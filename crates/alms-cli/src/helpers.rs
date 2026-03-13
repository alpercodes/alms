use alms_core::{AgentId, AgentRecord};
use alms_session::SqliteStore;

/// Open the SQLite store at the configured DB path.
pub(crate) fn open_db() -> anyhow::Result<SqliteStore> {
    let db_path = std::env::var("ALMS_DB_PATH").unwrap_or_else(|_| "./data/alms.db".to_string());
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(SqliteStore::open(&db_path)?)
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
pub(crate) fn parse_api_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(msg) = val
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(|m| m.as_str())
    {
        return format!("HTTP {status}: {msg}");
    }
    format!("HTTP {status}: {body}")
}

/// Send a GET request and return the response body as JSON Value.
pub(crate) async fn api_get(base_url: &str, path: &str) -> anyhow::Result<serde_json::Value> {
    let client = api_client()?;
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
    base_url: &str,
    path: &str,
    body: &impl serde::Serialize,
) -> anyhow::Result<(reqwest::StatusCode, serde_json::Value)> {
    let client = api_client()?;
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
pub(crate) async fn api_delete(base_url: &str, path: &str) -> anyhow::Result<reqwest::StatusCode> {
    let client = api_client()?;
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
        system_prompt: None,
        posture: None,
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
