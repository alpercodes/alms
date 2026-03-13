use alms_core::SessionId;
use alms_session::{Session, SqliteStore};
use clap::Subcommand;

use crate::helpers::{fmt_time, resolve_agent, short_id};

#[derive(Subcommand, Debug)]
pub(crate) enum SessionCommands {
    /// List sessions (optionally filtered by agent)
    List {
        /// Filter by agent name or UUID
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show details of a specific session
    Show {
        /// Session UUID
        session_id: String,
    },
    /// Delete a session and all its messages
    Delete {
        /// Session UUID
        session_id: String,
    },
}

pub(crate) fn session_list(
    store: &SqliteStore,
    agent: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let sessions: Vec<Session> = if let Some(ref name_or_id) = agent {
        let agent = resolve_agent(store, name_or_id)?;
        store.load_sessions_by_agent(agent.id)?
    } else {
        store.list_sessions()?
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&sessions)?);
        return Ok(());
    }
    if sessions.is_empty() {
        if let Some(ref a) = agent {
            println!("No sessions found for agent '{a}'.");
        } else {
            println!("No sessions found.");
        }
        return Ok(());
    }

    println!(
        "{:<12} {:<12} {:<10} {:<8} {:<22} LAST ACTIVITY",
        "SESSION", "AGENT", "STATUS", "MSGS", "CREATED"
    );
    for s in &sessions {
        let id_short = short_id(&s.id.0);
        let agent_short = short_id(&s.agent_id);
        let msg_count = store.message_count(s.id).unwrap_or(0);
        println!(
            "{:<12} {:<12} {:<10} {:<8} {:<22} {}",
            id_short,
            agent_short,
            s.status,
            msg_count,
            fmt_time(&s.created_at.0),
            fmt_time(&s.last_activity.0),
        );
    }
    Ok(())
}

pub(crate) fn session_show(
    store: &SqliteStore,
    session_id_str: &str,
    json: bool,
) -> anyhow::Result<()> {
    let uuid =
        uuid::Uuid::parse_str(session_id_str).map_err(|_| anyhow::anyhow!("Invalid UUID"))?;
    let sid = SessionId(uuid);
    let session = store
        .load_session_by_id(sid)?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id_str}"))?;

    let msg_count = store.message_count(sid).unwrap_or(0);

    if json {
        let mut val = serde_json::to_value(&session)?;
        val.as_object_mut()
            .unwrap()
            .insert("message_count".into(), serde_json::json!(msg_count));
        println!("{}", serde_json::to_string_pretty(&val)?);
        return Ok(());
    }

    println!("Session:       {}", session.id.0);
    println!("Agent:         {}", session.agent_id);
    if let Ok(Some(agent)) = store.load_agent_by_id(session.agent_id) {
        println!("Agent Name:    {}", agent.name);
    }
    println!("Context:       {}", session.context_id);
    println!("Status:        {}", session.status);
    println!("Messages:      {}", msg_count);
    println!("Created:       {}", fmt_time(&session.created_at.0));
    println!("Last Activity: {}", fmt_time(&session.last_activity.0));
    Ok(())
}

pub(crate) fn session_delete(
    store: &SqliteStore,
    session_id_str: &str,
    json: bool,
) -> anyhow::Result<()> {
    let uuid =
        uuid::Uuid::parse_str(session_id_str).map_err(|_| anyhow::anyhow!("Invalid UUID"))?;
    let sid = SessionId(uuid);

    // Verify session exists before deleting
    store
        .load_session_by_id(sid)?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id_str}"))?;

    store.delete_session(sid)?;

    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "deleted": session_id_str })
        );
    } else {
        println!("Deleted session {session_id_str}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{make_agent, new_store};
    use alms_session::Session as AlmsSession;

    fn make_session(store: &SqliteStore, agent_id: alms_core::AgentId) -> AlmsSession {
        let session = AlmsSession::new(agent_id, "default");
        store.save_session(&session).unwrap();
        session
    }

    #[test]
    fn test_session_list_empty() {
        let store = new_store();
        session_list(&store, None, false).unwrap();
    }

    #[test]
    fn test_session_list_all() {
        let store = new_store();
        let agent = make_agent(&store, "sess-agent");
        make_session(&store, agent.id);
        make_session(&store, agent.id);

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn test_session_list_by_agent() {
        let store = new_store();
        let a1 = make_agent(&store, "agent-a");
        let a2 = make_agent(&store, "agent-b");
        make_session(&store, a1.id);
        make_session(&store, a1.id);
        make_session(&store, a2.id);

        let s1 = store.load_sessions_by_agent(a1.id).unwrap();
        assert_eq!(s1.len(), 2);
        let s2 = store.load_sessions_by_agent(a2.id).unwrap();
        assert_eq!(s2.len(), 1);
    }

    #[test]
    fn test_session_show() {
        let store = new_store();
        let agent = make_agent(&store, "show-agent");
        let session = make_session(&store, agent.id);

        session_show(&store, &session.id.0.to_string(), false).unwrap();
    }

    #[test]
    fn test_session_show_not_found() {
        let store = new_store();
        let err = session_show(&store, &uuid::Uuid::new_v4().to_string(), false).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_session_show_invalid_uuid() {
        let store = new_store();
        let err = session_show(&store, "not-a-uuid", false).unwrap_err();
        assert!(err.to_string().contains("Invalid UUID"));
    }

    #[test]
    fn test_session_delete() {
        let store = new_store();
        let agent = make_agent(&store, "del-agent");
        let session = make_session(&store, agent.id);

        session_delete(&store, &session.id.0.to_string(), false).unwrap();
        assert!(store.load_session_by_id(session.id).unwrap().is_none());
    }

    #[test]
    fn test_session_delete_not_found() {
        let store = new_store();
        let err = session_delete(&store, &uuid::Uuid::new_v4().to_string(), false).unwrap_err();
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_session_list_json() {
        let store = new_store();
        let agent = make_agent(&store, "json-agent");
        make_session(&store, agent.id);
        // Just verify it doesn't panic — JSON serialization works
        session_list(&store, None, true).unwrap();
    }

    #[test]
    fn test_session_show_json() {
        let store = new_store();
        let agent = make_agent(&store, "json-show-agent");
        let session = make_session(&store, agent.id);
        // Exercises the as_object_mut().unwrap() path — verifies Session
        // serializes to a JSON object (not array/primitive)
        session_show(&store, &session.id.0.to_string(), true).unwrap();
    }

    #[test]
    fn test_session_delete_json() {
        let store = new_store();
        let agent = make_agent(&store, "json-del-agent");
        let session = make_session(&store, agent.id);
        session_delete(&store, &session.id.0.to_string(), true).unwrap();
        assert!(store.load_session_by_id(session.id).unwrap().is_none());
    }
}
