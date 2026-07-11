-- Frozen schema fixture from v0.2.4-pre-stabilization (eadd4b3).
-- Keep this independent from production schema constants so upgrade tests
-- detect accidental incompatibility.

PRAGMA foreign_keys=ON;

CREATE TABLE sessions (
    id            TEXT PRIMARY KEY,
    agent_id      TEXT NOT NULL,
    context_id    TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    last_activity TEXT NOT NULL,
    status        TEXT NOT NULL
);

CREATE TABLE messages (
    id         TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    role       TEXT NOT NULL,
    content    TEXT NOT NULL,
    timestamp  TEXT NOT NULL,
    metadata   TEXT,
    seq        INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_messages_session_seq ON messages(session_id, seq);

CREATE TABLE audit_events (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    run_id     TEXT,
    tool       TEXT NOT NULL,
    decision   TEXT NOT NULL,
    params     TEXT NOT NULL,
    result     TEXT,
    error      TEXT,
    ts         TEXT NOT NULL
);

CREATE TABLE jobs (
    id          TEXT PRIMARY KEY,
    agent_id    TEXT NOT NULL,
    prompt      TEXT NOT NULL,
    schedule    TEXT NOT NULL,
    status      TEXT NOT NULL DEFAULT 'pending',
    created_at  TEXT NOT NULL,
    next_run_at TEXT,
    last_run_at TEXT
);

CREATE TABLE context_summaries (
    session_id       TEXT PRIMARY KEY REFERENCES sessions(id),
    text             TEXT NOT NULL DEFAULT '',
    messages_covered INTEGER NOT NULL DEFAULT 0,
    updated_at       TEXT
);

CREATE TABLE agents (
    id                     TEXT PRIMARY KEY,
    name                   TEXT NOT NULL UNIQUE,
    description            TEXT NOT NULL DEFAULT '',
    model                  TEXT,
    system_prompt          TEXT,
    posture                TEXT,
    provider               TEXT,
    telegram_token         TEXT,
    is_default             INTEGER NOT NULL DEFAULT 0,
    created_at             TEXT NOT NULL,
    last_active            TEXT NOT NULL,
    thinking_budget_tokens INTEGER,
    reasoning_effort       TEXT,
    gemini_thinking_budget INTEGER,
    summary_provider       TEXT,
    summary_model          TEXT,
    worktree_mode          TEXT,
    debug_mode             INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_agents_is_default ON agents(is_default);

CREATE TABLE runs (
    run_id            TEXT PRIMARY KEY,
    session_id        TEXT NOT NULL,
    agent_id          TEXT NOT NULL,
    input             TEXT NOT NULL DEFAULT '',
    response          TEXT,
    error             TEXT,
    status            TEXT NOT NULL DEFAULT 'queued',
    started_at        TEXT,
    ended_at          TEXT,
    prompt_tokens     INTEGER,
    completion_tokens INTEGER,
    job_id            TEXT,
    parent_run_id     TEXT,
    created_at        TEXT NOT NULL,
    resolved_config   TEXT
);

CREATE INDEX idx_runs_session_id ON runs(session_id);
CREATE INDEX idx_runs_agent_id ON runs(agent_id);

CREATE TABLE run_tool_calls (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id     TEXT NOT NULL,
    session_id TEXT,
    seq        INTEGER NOT NULL,
    role       TEXT NOT NULL,
    tool_name  TEXT,
    tool_id    TEXT,
    params     TEXT,
    result     TEXT,
    timestamp  TEXT NOT NULL,
    from_agent TEXT
);

CREATE INDEX idx_run_tool_calls_run ON run_tool_calls(run_id, seq);
CREATE INDEX idx_run_tool_calls_session ON run_tool_calls(session_id);

CREATE TABLE session_summaries (
    agent_id     TEXT NOT NULL,
    session_id   TEXT NOT NULL REFERENCES sessions(id),
    summary      TEXT NOT NULL DEFAULT '',
    last_run_id  TEXT,
    updated_at   TEXT NOT NULL,
    source_label TEXT,
    PRIMARY KEY (agent_id, session_id)
);

CREATE INDEX idx_session_summaries_agent
    ON session_summaries(agent_id, updated_at DESC);
