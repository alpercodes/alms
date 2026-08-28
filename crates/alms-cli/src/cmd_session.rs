use alms_core::{SessionId, classify_session_type, parse_subagent_parent};
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

/// Header row for the `alms session list` table.
///
/// Kept next to [`format_session_row`] because the two must agree on
/// column widths.
fn session_list_header() -> String {
    format!(
        "{:<12} {:<12} {:<13} {:<10} {:<8} {:<22} LAST ACTIVITY",
        "SESSION", "AGENT", "TYPE", "STATUS", "MSGS", "CREATED"
    )
}

/// Render one row of the `alms session list` table.
///
/// The `TYPE` cell comes from [`classify_session_type`] — the same single
/// source of truth `GET /sessions` and the `list_my_sessions` tool use — so
/// an operator can tell a chat they can run against from an internal
/// transcript they cannot. See [`session_list`] for why this listing shows
/// internal rows at all.
fn format_session_row(session: &Session, msg_count: usize) -> String {
    format!(
        "{:<12} {:<12} {:<13} {:<10} {:<8} {:<22} {}",
        short_id(&session.id.0),
        short_id(&session.agent_id),
        classify_session_type(&session.context_id),
        session.status,
        msg_count,
        fmt_time(&session.created_at.0),
        fmt_time(&session.last_activity.0),
    )
}

/// Serialize one session for `alms session list --json`.
///
/// Carries the same two discriminating fields `GET /sessions` adds, under
/// the same names, so a script reading either surface branches the same
/// way:
///
/// - `session_type` — always present, from [`classify_session_type`].
/// - `parent_agent_id` — subagent sessions only, the agent that *invoked*
///   the subagent, recovered from the `context_id` (#1278). Omitted when
///   the context carries no readable parent, and omitted entirely for
///   every other session type: presence is a usable "is this a subagent
///   row" test, so it must not appear on rows that aren't.
///
/// The table's `TYPE` column is the human half of this; the `--json` half
/// exists because `alms session list --agent X --json` is the scripting
/// path and would otherwise be the one surface with no way at all to tell
/// a subagent transcript from a chat.
fn enrich_session_json(session: &Session) -> anyhow::Result<serde_json::Value> {
    let mut val = serde_json::to_value(session)?;
    let session_type = classify_session_type(&session.context_id);
    if let Some(obj) = val.as_object_mut() {
        obj.insert("session_type".into(), serde_json::json!(session_type));
        if session_type == "subagent"
            && let Some(parent) = parse_subagent_parent(&session.context_id)
        {
            obj.insert(
                "parent_agent_id".into(),
                serde_json::json!(parent.0.to_string()),
            );
        }
    }
    Ok(val)
}

/// The agent that *invoked* a subagent, for `alms session show`.
///
/// Resolved from the `context_id` — never from `session.agent_id`, which
/// since #1278 names the agent that *ran* the subagent, i.e. the other
/// party. Returns the parent's registered name when it resolves, its UUID
/// when it does not, and `None` when the context carries no parseable
/// parent at all.
///
/// The UUID fallback covers **three** causes, not two: a deleted agent, a
/// session written by a different tenant's database, and a registry read
/// that *failed* (Tim N1 on #1295). The third is folded in deliberately —
/// the UUID is a value the operator could legitimately see for either of
/// the first two, so it is not a fabricated answer, and a `session show`
/// that aborted on a registry hiccup would be worse than one that names
/// the parent less precisely. The arm is kept collapsed and the cause
/// named here rather than split, because nothing downstream branches on
/// which of the three it was.
fn invoking_parent_label(store: &SqliteStore, context_id: &str) -> Option<String> {
    let parent = parse_subagent_parent(context_id)?;
    match store.load_agent_by_id(parent) {
        Ok(Some(agent)) => Some(agent.name),
        // Ok(None) = deleted / foreign; Err(_) = registry unreadable.
        _ => Some(parent.0.to_string()),
    }
}

/// Select the rows `alms session list` shows.
///
/// **Deliberately uncurated — this does not adopt the exclusions
/// `GET /sessions` applies.** #1289 asked whether it should, after #1278
/// filed named subagent sessions under the invoked agent's registry id
/// and so made them visible on the `--agent` path for the first time.
///
/// The shortest form of the answer is that on the path #1289 actually
/// asked about there is **nothing to curate**: the CLI and the HTTP arm
/// already return the *same* subagent rows there. Named in, ephemeral
/// out — HTTP by its explicit `Some(SubagentOwner::Named(_))` filter,
/// this path by the `WHERE agent_id = ?1` clause, arriving at the same
/// set by different mechanisms. Pinned by
/// `test_session_list_by_agent_is_scoped_to_rows_filed_under_that_agent`.
///
/// And the second: **this path never curated anything to begin with.**
/// `load_sessions_by_agent` is `SELECT ... WHERE agent_id = ?1` with no
/// type filter, so episodic, notification and job rows filed under an
/// agent have always listed here. #1278 added one more category to a
/// listing that has never had an exclusion in it. "Do not start now" is a
/// much smaller claim than "decline to adopt the HTTP exclusions".
///
/// Then, for the exclusions themselves — three reasons not to import them:
///
/// 1. They are *sidebar* decisions, not correctness ones. Ephemeral rows
///    are dropped there for two reasons, and the load-bearing one is
///    structural: **an ephemeral subagent has no registry agent, so there
///    is no timeline for it to appear in** — which is the same reason
///    this path cannot see them either, making the parallel exact rather
///    than coincidental. (The second reason is sidebar noise: one
///    permanent row per one-shot `invoke_agent` call.) The other
///    subagent-row exclusion is `filterChatSessions` dropping them from
///    the per-agent scope so the boot flow cannot auto-select a read-only
///    transcript. The CLI has no sidebar, no boot flow and no
///    auto-select; none of those costs exist here, and hiding rows from
///    an operator's inspection tool is a cost that does.
/// 2. Internal consistency. `alms session list` with no `--agent` already
///    returns every row in the table via `list_sessions()` — DM, episodic,
///    ephemeral subagent, all of it. Curating only the `--agent` path
///    would make the *narrower* query show *fewer kinds* of row than the
///    broader one.
/// 3. `alms session show` and `--json` are the drill-down, so the listing
///    only has to be legible, not selective. That is what the `TYPE`
///    column (and the `--json` `session_type` field) are for: before
///    #1289 nothing in the output distinguished a subagent transcript from
///    a chat, which is the gap that actually mattered.
///
/// **One of the HTTP exclusions is not sidebar-shaped, and is complied
/// with rather than declined.** The gateway also drops a subagent context
/// it cannot *parse*, on #1277's rule that an unreadable owner must never
/// be guessed at — a display-safety rule with no sidebar in it. This
/// listing satisfies that rule without excluding the row, because it
/// never guesses: [`invoking_parent_label`] returns `None` so no
/// `Invoked By` line prints, [`enrich_session_json`] omits
/// `parent_agent_id`, and the `TYPE` cell is prefix-derived and so true
/// of every shape. Showing the row while naming no owner is the outcome
/// #1277 wanted; the gateway excludes it only because a sidebar row has
/// nowhere to put "owner unknown".
///
/// `--agent X` means **filed under X** and nothing wider. An agent's own
/// subagent transcripts are in scope because #1278 files them under it;
/// the transcripts it *invoked* on other agents are not, because they are
/// filed under those agents. Two corollaries, both pinned:
///
/// - #1289's premise that this path lists "ephemeral ones the HTTP arm
///   deliberately excludes" is wrong — it cannot.
///   `derive_subagent_identity` files an ephemeral subagent under a fresh
///   `AgentId::new()`, which is nobody's registry id, so the
///   `WHERE agent_id = ?` behind `load_sessions_by_agent` never selects
///   one. The rows #1278 made newly visible here are exactly the
///   **named** ones — the same rows the HTTP arm includes.
/// - Widening this to "sessions X invoked as well as ran" would be a
///   different, larger feature (and would need to answer #1181's
///   parent-ownership question), not a fix to this one.
fn load_listed_sessions(store: &SqliteStore, agent: Option<&str>) -> anyhow::Result<Vec<Session>> {
    match agent {
        Some(name_or_id) => {
            let agent = resolve_agent(store, name_or_id)?;
            Ok(store.load_sessions_by_agent(agent.id)?)
        }
        None => Ok(store.list_sessions()?),
    }
}

/// Everything `alms session list` emits, as a string.
///
/// The command itself is one `println!` of this, deliberately: the crate
/// has no stdout-capture harness, so anything left inside `session_list`
/// is unreachable by a test (Tim on #1295). Selection, curation, the
/// enrichment the `--json` branch applies and the columns the table
/// branch prints all live here, where the surface tests can reach them —
/// including the one decision this whole item is about, which is that
/// [`load_listed_sessions`] does not filter and neither does the loop
/// below.
fn render_session_list(
    store: &SqliteStore,
    agent: Option<&str>,
    json: bool,
) -> anyhow::Result<String> {
    let sessions = load_listed_sessions(store, agent)?;

    if json {
        let enriched = sessions
            .iter()
            .map(enrich_session_json)
            .collect::<anyhow::Result<Vec<_>>>()?;
        return Ok(serde_json::to_string_pretty(&enriched)?);
    }
    if sessions.is_empty() {
        return Ok(match agent {
            Some(a) => format!("No sessions found for agent '{a}'."),
            None => "No sessions found.".to_string(),
        });
    }

    let mut out = session_list_header();
    for s in &sessions {
        let msg_count = store.message_count(s.id).unwrap_or(0);
        out.push('\n');
        out.push_str(&format_session_row(s, msg_count));
    }
    Ok(out)
}

/// `alms session list [--agent <name|uuid>]`.
pub(crate) fn session_list(
    store: &SqliteStore,
    agent: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    println!("{}", render_session_list(store, agent.as_deref(), json)?);
    Ok(())
}

/// Everything `alms session show` emits, as a string. Same reasoning as
/// [`render_session_list`].
fn render_session_show(
    store: &SqliteStore,
    session_id_str: &str,
    json: bool,
) -> anyhow::Result<String> {
    let uuid =
        uuid::Uuid::parse_str(session_id_str).map_err(|_| anyhow::anyhow!("Invalid UUID"))?;
    let sid = SessionId(uuid);
    let session = store
        .load_session_by_id(sid)?
        .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id_str}"))?;

    let msg_count = store.message_count(sid).unwrap_or(0);

    if json {
        let mut val = enrich_session_json(&session)?;
        val.as_object_mut()
            .unwrap()
            .insert("message_count".into(), serde_json::json!(msg_count));
        return Ok(serde_json::to_string_pretty(&val)?);
    }

    let mut out = String::new();
    out.push_str(&format!("Session:       {}\n", session.id.0));
    out.push_str(&format!("Agent:         {}\n", session.agent_id));
    if let Ok(Some(agent)) = store.load_agent_by_id(session.agent_id) {
        out.push_str(&format!("Agent Name:    {}\n", agent.name));
    }
    out.push_str(&format!("Context:       {}\n", session.context_id));
    // The two halves of a subagent session's identity name different
    // agents (#1278): `Agent`/`Agent Name` above is the agent that RAN it,
    // the parent below is the one that INVOKED it. The listing's `TYPE`
    // column says a row is a subagent transcript; this says whose.
    if let Some(parent) = invoking_parent_label(store, &session.context_id) {
        out.push_str(&format!("Invoked By:    {parent}\n"));
    }
    out.push_str(&format!("Status:        {}\n", session.status));
    out.push_str(&format!("Messages:      {msg_count}\n"));
    out.push_str(&format!(
        "Created:       {}\n",
        fmt_time(&session.created_at.0)
    ));
    out.push_str(&format!(
        "Last Activity: {}",
        fmt_time(&session.last_activity.0)
    ));
    Ok(out)
}

pub(crate) fn session_show(
    store: &SqliteStore,
    session_id_str: &str,
    json: bool,
) -> anyhow::Result<()> {
    println!("{}", render_session_show(store, session_id_str, json)?);
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

    /// Save a session with an explicit `context_id`.
    fn make_session_with_context(
        store: &SqliteStore,
        agent_id: alms_core::AgentId,
        context_id: &str,
    ) -> AlmsSession {
        let session = AlmsSession::new(agent_id, context_id);
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

    // ---------------------------------------------------------------
    // #1289 item 1: subagent rows on the `alms session list` surfaces
    //
    // The `render_*` tests below are the SURFACE rows (Tim S1 on #1295).
    // Everything `session_list` / `session_show` do is one `println!` of
    // a renderer, so a mutation anywhere between the store and the bytes
    // an operator sees — a curation filter in the print loop, an inlined
    // `format!` that forgets the TYPE column, a `--json` branch that
    // reaches for `serde_json::to_value` instead of `enrich_session_json`
    // — lands inside a function these call. The helper tests that follow
    // stay because they localise WHICH part broke.
    // ---------------------------------------------------------------

    /// Fixture: an agent with one chat and one subagent transcript filed
    /// under it, plus the agent that invoked the latter.
    fn store_with_a_subagent_transcript() -> (SqliteStore, AlmsSession, AlmsSession) {
        let store = new_store();
        let reviewer = make_agent(&store, "reviewer");
        let atlas = make_agent(&store, "atlas");
        let chat = make_session(&store, reviewer.id);
        let sub = make_session_with_context(
            &store,
            reviewer.id,
            &format!("subagent_{}_reviewer", atlas.id.0),
        );
        (store, chat, sub)
    }

    /// Surface row for the table branch. Closes three call-site
    /// mutations at once: a curation filter added anywhere in the list
    /// path (the subagent row must still be there), an inlined `format!`
    /// that drops the `TYPE` column, and an inlined header literal.
    #[test]
    fn test_render_session_list_table_shows_and_labels_a_subagent_row() {
        let (store, _chat, sub) = store_with_a_subagent_transcript();

        let out = render_session_list(&store, Some("reviewer"), false).unwrap();

        assert!(
            out.contains("TYPE"),
            "the table must carry a TYPE heading; got:\n{out}"
        );
        let sub_row = out
            .lines()
            .find(|l| l.starts_with(&short_id(&sub.id.0)))
            .unwrap_or_else(|| panic!("the subagent row must be listed, uncurated; got:\n{out}"));
        assert!(
            sub_row.contains("subagent"),
            "and must be labelled as one; got {sub_row:?}"
        );
        assert_eq!(
            out.lines().count(),
            3,
            "header + the agent's chat + its subagent transcript; got:\n{out}"
        );
    }

    /// Surface row for the `--json` branch: it must go through
    /// `enrich_session_json`, not raw `serde_json::to_value`.
    #[test]
    fn test_render_session_list_json_carries_the_enriched_fields() {
        let (store, _chat, sub) = store_with_a_subagent_transcript();

        let out = render_session_list(&store, Some("reviewer"), true).unwrap();
        let rows: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
        let row = rows
            .iter()
            .find(|r| r["id"] == sub.id.0.to_string())
            .unwrap_or_else(|| panic!("the subagent row must be present; got {out}"));

        assert_eq!(row["session_type"], "subagent", "got {out}");
        assert!(
            row.get("parent_agent_id").is_some(),
            "the --json branch must be enriched, not a raw Session dump; got {out}"
        );
    }

    /// Surface row for `session show`: the `Invoked By` block must
    /// actually be emitted, and only for a subagent session.
    #[test]
    fn test_render_session_show_names_the_invoking_parent() {
        let (store, chat, sub) = store_with_a_subagent_transcript();

        let sub_out = render_session_show(&store, &sub.id.0.to_string(), false).unwrap();
        assert!(
            sub_out.contains("Invoked By:"),
            "a subagent session must show its invoker; got:\n{sub_out}"
        );
        assert!(
            sub_out.contains("atlas"),
            "named as the INVOKING agent; got:\n{sub_out}"
        );
        assert!(
            sub_out.contains("Agent Name:    reviewer"),
            "beside the agent that RAN it; got:\n{sub_out}"
        );

        let chat_out = render_session_show(&store, &chat.id.0.to_string(), false).unwrap();
        assert!(
            !chat_out.contains("Invoked By"),
            "an ordinary chat has no invoker; got:\n{chat_out}"
        );
    }

    /// `session show --json` routes through the same enrichment, so it
    /// gained the two new fields as well (Tim N2) — pinned rather than
    /// left as an undocumented side effect.
    #[test]
    fn test_render_session_show_json_carries_the_enriched_fields() {
        let (store, _chat, sub) = store_with_a_subagent_transcript();

        let out = render_session_show(&store, &sub.id.0.to_string(), true).unwrap();
        let val: serde_json::Value = serde_json::from_str(&out).unwrap();

        assert_eq!(val["session_type"], "subagent", "got {out}");
        assert!(val.get("parent_agent_id").is_some(), "got {out}");
        assert!(
            val.get("message_count").is_some(),
            "and must keep the field `show --json` already had; got {out}"
        );
    }

    /// `--agent X` means **filed under X** — no narrower, no wider.
    ///
    /// Narrower: the named subagent transcript X *ran* is in scope, and
    /// `load_listed_sessions` must not adopt the HTTP arm's exclusions and
    /// drop it. Wider: the ephemeral transcript X *invoked* is filed under
    /// a fresh `AgentId::new()` and must stay out — which is also the
    /// evidence against #1289's premise that this path lists "the
    /// ephemeral ones the HTTP arm deliberately excludes". It cannot: the
    /// `WHERE agent_id = ?` behind this query never selects one.
    #[test]
    fn test_session_list_by_agent_is_scoped_to_rows_filed_under_that_agent() {
        let store = new_store();
        let reviewer = make_agent(&store, "reviewer");
        let atlas = make_agent(&store, "atlas");

        let chat = make_session(&store, reviewer.id);
        // Named, RUN by reviewer: filed under the invoked agent's registry
        // id (#1278), context names atlas as the invoker.
        let ran = make_session_with_context(
            &store,
            reviewer.id,
            &format!("subagent_{}_reviewer", atlas.id.0),
        );
        // Ephemeral, INVOKED by reviewer: fresh AgentId, task id trailing.
        let invoked = make_session_with_context(
            &store,
            alms_core::AgentId::new(),
            &format!("subagent_{}_{}", reviewer.id.0, uuid::Uuid::new_v4()),
        );

        let rows = load_listed_sessions(&store, Some("reviewer")).unwrap();
        let ids: Vec<_> = rows.iter().map(|s| s.id).collect();

        assert!(ids.contains(&chat.id), "the agent's own chat must list");
        assert!(
            ids.contains(&ran.id),
            "the subagent transcript this agent RAN is filed under it (#1278) \
             and must list — this listing does not adopt the HTTP arm's \
             exclusions; got {ids:?}"
        );
        assert!(
            !ids.contains(&invoked.id),
            "the subagent transcript this agent INVOKED is filed under a fresh \
             AgentId, not under it, and must not list; got {ids:?}"
        );
    }

    /// The unfiltered listing is the raw view and stays that way: it
    /// returns every row in the table, which is reason 2 the `--agent`
    /// path does not curate either.
    #[test]
    fn test_session_list_without_an_agent_filter_returns_every_row() {
        let store = new_store();
        let agent = make_agent(&store, "raw");
        let chat = make_session(&store, agent.id);
        let episodic = make_session_with_context(
            &store,
            agent.id,
            &format!("episodic:{}", uuid::Uuid::new_v4()),
        );
        let ephemeral = make_session_with_context(
            &store,
            alms_core::AgentId::new(),
            &format!("subagent_{}_{}", agent.id.0, uuid::Uuid::new_v4()),
        );

        let ids: Vec<_> = load_listed_sessions(&store, None)
            .unwrap()
            .iter()
            .map(|s| s.id)
            .collect();

        for (label, id) in [
            ("chat", chat.id),
            ("episodic", episodic.id),
            ("ephemeral subagent", ephemeral.id),
        ] {
            assert!(
                ids.contains(&id),
                "the unfiltered listing must return the {label} row; got {ids:?}"
            );
        }
    }

    /// The gap #1289 item 1 actually closes: before it, the table said
    /// nothing about what kind of session a row was, so a subagent
    /// transcript and a chat rendered identically.
    #[test]
    fn test_session_row_renders_the_session_type() {
        let store = new_store();
        let reviewer = make_agent(&store, "reviewer");
        let parent = make_agent(&store, "atlas");

        let chat = make_session(&store, reviewer.id);
        let sub = make_session_with_context(
            &store,
            reviewer.id,
            &format!("subagent_{}_reviewer", parent.id.0),
        );

        let chat_row = format_session_row(&chat, 3);
        let sub_row = format_session_row(&sub, 7);

        assert!(
            sub_row.contains("subagent"),
            "a subagent row must say so; got {sub_row:?}"
        );
        assert!(
            chat_row.contains("chat"),
            "a chat row must say so; got {chat_row:?}"
        );
        assert!(
            !chat_row.contains("subagent"),
            "a chat row must not be labelled a subagent; got {chat_row:?}"
        );
        assert!(
            session_list_header().contains("TYPE"),
            "the header must name the column the rows fill"
        );
    }

    /// Header and rows are formatted by two separate `format!` strings, so
    /// nothing but a test stops one drifting from the other and shearing
    /// every column after the change.
    #[test]
    fn test_session_list_header_and_row_columns_line_up() {
        let store = new_store();
        let agent = make_agent(&store, "aligned");
        let session = make_session(&store, agent.id);

        let header = session_list_header();
        let row = format_session_row(&session, 0);
        let status_col = header.find("STATUS").expect("header has a STATUS column");

        assert_eq!(
            row.get(status_col..status_col + session.status.to_string().len()),
            Some(session.status.to_string().as_str()),
            "the STATUS cell must start under the STATUS heading;\n{header}\n{row}"
        );
    }

    /// `--json` is the scripting path, and it is the surface that would
    /// otherwise have had no way at all to tell a subagent transcript from
    /// a chat. It carries the same two fields `GET /sessions` adds, under
    /// the same names.
    #[test]
    fn test_session_json_carries_type_and_invoking_parent() {
        let store = new_store();
        let reviewer = make_agent(&store, "reviewer");
        let parent = make_agent(&store, "atlas");

        let sub = make_session_with_context(
            &store,
            reviewer.id,
            &format!("subagent_{}_reviewer", parent.id.0),
        );
        let val = enrich_session_json(&sub).unwrap();

        assert_eq!(val["session_type"], "subagent");
        assert_eq!(
            val["parent_agent_id"],
            parent.id.0.to_string(),
            "parent_agent_id must name the INVOKING agent, not the one the \
             session is filed under; got {val}"
        );
        assert_eq!(
            val["agent_id"],
            reviewer.id.0.to_string(),
            "agent_id must still be the agent that RAN the subagent (#1278)"
        );

        // Presence is a usable "is this a subagent row" test, so the field
        // must be absent on every other type.
        let chat = enrich_session_json(&make_session(&store, reviewer.id)).unwrap();
        assert_eq!(chat["session_type"], "chat");
        assert!(
            chat.get("parent_agent_id").is_none(),
            "parent_agent_id must not appear on a non-subagent row; got {chat}"
        );
    }

    /// A subagent `context_id` this binary cannot parse (the legacy
    /// pre-#1185 `subagent_{task_id}` shape has no parent segment) must
    /// not be guessed at — #1277's rule.
    #[test]
    fn test_session_json_omits_parent_for_an_unparseable_subagent_context() {
        let store = new_store();
        let agent = make_agent(&store, "legacy");
        let legacy = make_session_with_context(
            &store,
            agent.id,
            &format!("subagent_{}", uuid::Uuid::new_v4()),
        );

        let val = enrich_session_json(&legacy).unwrap();
        assert_eq!(val["session_type"], "subagent");
        assert!(
            val.get("parent_agent_id").is_none(),
            "an unreadable parent must be omitted, not invented; got {val}"
        );
    }

    /// `session show` resolves the invoking parent to its registered name.
    /// The UUID is already on the `Context:` line, so the name is the
    /// whole value added.
    #[test]
    fn test_invoking_parent_label_resolves_the_registered_name() {
        let store = new_store();
        let parent = make_agent(&store, "atlas");

        assert_eq!(
            invoking_parent_label(&store, &format!("subagent_{}_reviewer", parent.id.0)),
            Some("atlas".to_string()),
            "a registered parent must render as its name"
        );

        // Unregistered parent: fall back to the id rather than dropping
        // the line — the operator still learns there was a parent.
        let orphan = alms_core::AgentId::new();
        assert_eq!(
            invoking_parent_label(&store, &format!("subagent_{}_reviewer", orphan.0)),
            Some(orphan.0.to_string())
        );

        // Not a subagent session, and an unparseable one: no line at all.
        assert_eq!(invoking_parent_label(&store, "web-chat"), None);
        assert_eq!(
            invoking_parent_label(&store, &format!("subagent_{}", uuid::Uuid::new_v4())),
            None
        );
    }

    /// The label must be read out of the `context_id`, never out of
    /// `session.agent_id` — since #1278 the latter names the agent that
    /// RAN the subagent, which is the other party. A self-invocation is
    /// the one case where the two agree, so it cannot catch this; two
    /// distinct agents can.
    #[test]
    fn test_invoking_parent_label_does_not_name_the_running_agent() {
        let store = new_store();
        let reviewer = make_agent(&store, "reviewer");
        let parent = make_agent(&store, "atlas");

        let sub = make_session_with_context(
            &store,
            reviewer.id,
            &format!("subagent_{}_reviewer", parent.id.0),
        );

        assert_eq!(
            invoking_parent_label(&store, &sub.context_id),
            Some("atlas".to_string()),
            "must name the invoker, not the runner"
        );
        assert_ne!(
            invoking_parent_label(&store, &sub.context_id),
            Some("reviewer".to_string())
        );
    }
}
