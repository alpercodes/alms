use alms_core::{AgentId, AgentRecord, WorktreeMode, config::ReasoningEffort, validate_agent_name};
use alms_session::SqliteStore;
use clap::{Subcommand, ValueEnum};

use crate::helpers::{fmt_time, resolve_agent, short_id};

/// Clap-compatible wrapper around [`alms_core::config::ReasoningEffort`].
///
/// Defined here (rather than deriving `ValueEnum` on the core type) so that
/// the core crate stays clap-free. The conversion into the canonical
/// `ReasoningEffort` is a single `From` impl.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum ReasoningEffortArg {
    Minimal,
    Low,
    Medium,
    High,
}

impl From<ReasoningEffortArg> for ReasoningEffort {
    fn from(v: ReasoningEffortArg) -> Self {
        match v {
            ReasoningEffortArg::Minimal => ReasoningEffort::Minimal,
            ReasoningEffortArg::Low => ReasoningEffort::Low,
            ReasoningEffortArg::Medium => ReasoningEffort::Medium,
            ReasoningEffortArg::High => ReasoningEffort::High,
        }
    }
}

/// Clap-compatible wrapper around [`WorktreeMode`] (#946).
///
/// Defined here (rather than deriving `ValueEnum` on the core type) so
/// that the core crate stays clap-free — same shape as
/// [`ReasoningEffortArg`].
#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum WorktreeModeArg {
    Off,
    Git,
}

impl From<WorktreeModeArg> for WorktreeMode {
    fn from(v: WorktreeModeArg) -> Self {
        match v {
            WorktreeModeArg::Off => WorktreeMode::Off,
            WorktreeModeArg::Git => WorktreeMode::Git,
        }
    }
}

#[derive(Subcommand, Debug)]
pub(crate) enum AgentCommands {
    /// List all registered agents
    List,
    /// Create a new agent
    Create {
        /// Agent name slug (lowercase, hyphens, 1-64 chars)
        name: String,
        /// Agent description
        #[arg(long)]
        description: Option<String>,
        /// Model override
        #[arg(long)]
        model: Option<String>,
        /// Posture override ("guarded", "full_control", or "autonomous")
        #[arg(long)]
        posture: Option<String>,
        /// LLM provider override ("openai", "anthropic", "openrouter")
        #[arg(long)]
        provider: Option<String>,
        /// Anthropic extended-thinking budget in tokens (Claude 4.x).
        /// `0` explicitly disables thinking for this agent even when the
        /// server default enables it. Omit the flag to inherit the server
        /// default (two-layer precedence: per-agent > server).
        #[arg(long)]
        thinking_budget_tokens: Option<u32>,
        /// OpenAI-compat reasoning effort for reasoning models (OpenAI
        /// o-series / GPT-5 / xAI Grok). Omit to inherit the server
        /// default from `[llm.openai].reasoning_effort`. DeepSeek R1
        /// ignores this flag — reasoning fires automatically on
        /// `deepseek-reasoner`. Non-reasoning OpenAI models (gpt-4o etc.)
        /// also ignore it. (#768)
        #[arg(long, value_enum)]
        reasoning_effort: Option<ReasoningEffortArg>,
        /// Gemini extended-thinking budget in tokens (Gemini 2.5+).
        /// `0` explicitly disables thinking for this agent even when the
        /// server default enables it. Omit the flag to inherit the server
        /// default (two-layer precedence: per-agent > server).
        /// Non-Gemini providers silently ignore the value. (#794)
        #[arg(long)]
        gemini_thinking_budget: Option<u32>,
        /// Per-agent summary-task provider override (#872, #876). Must be
        /// set together with `--summary-model`; setting one without the
        /// other is rejected with `SUMMARY_PROVIDER_REQUIRES_MODEL` /
        /// `SUMMARY_MODEL_REQUIRES_PROVIDER`. Omit both flags to inherit
        /// the server-level `[context].summary_provider` /
        /// `[context].summary_model`.
        #[arg(long)]
        summary_provider: Option<String>,
        /// Per-agent summary-task model override (#872, #876). Must be set
        /// together with `--summary-provider`. The model slug belongs to
        /// the summary provider's namespace, not the agent's primary
        /// provider — pairing a model slug with the wrong provider is
        /// rejected at create/PATCH time.
        #[arg(long)]
        summary_model: Option<String>,
        /// Per-agent git worktree-isolation mode (#946). `off`
        /// (default) → sandbox root is the project root. `git` → at
        /// agent-create time the CLI runs `git worktree add
        /// <project>/.alms/worktrees/<name>/ -b alms/<name>` and pins
        /// the agent's sandbox at that worktree path. Multiple
        /// agents on `git` mode can work in parallel on the same
        /// repo without filesystem collisions. Requires the project
        /// to be a git working tree; non-git projects fail with a
        /// clear error and the agent record is NOT persisted.
        #[arg(long, value_enum)]
        worktree_mode: Option<WorktreeModeArg>,
        /// Set as the default agent
        #[arg(long)]
        default: bool,
    },
    /// Show details of a specific agent
    Show {
        /// Agent name slug or UUID
        name_or_id: String,
    },
    /// Delete an agent
    Delete {
        /// Agent name slug or UUID
        name_or_id: String,
        /// Force delete even if the agent is the default
        #[arg(long)]
        force: bool,
    },
    /// Set an agent as the default
    SetDefault {
        /// Agent name slug or UUID
        name_or_id: String,
    },
    /// Update an agent's configuration
    Config {
        /// Agent name slug or UUID
        name_or_id: String,
        /// Model override (empty string to clear)
        #[arg(long)]
        model: Option<String>,
        /// Posture override (empty string to clear)
        #[arg(long)]
        posture: Option<String>,
        /// LLM provider override (empty string to clear)
        #[arg(long)]
        provider: Option<String>,
        /// Description
        #[arg(long)]
        description: Option<String>,
        /// Anthropic extended-thinking budget in tokens (Claude 4.x).
        /// `0` explicitly disables thinking for this agent. Omit the flag
        /// to leave the current value unchanged. Note: there is no CLI
        /// sentinel to clear back to "inherit server default" today —
        /// matches the HTTP API semantics (see PR #775 S3).
        #[arg(long)]
        thinking_budget_tokens: Option<u32>,
        /// OpenAI-compat reasoning effort override (#768). Omit to leave
        /// the current value unchanged; no sentinel to clear the override.
        #[arg(long, value_enum)]
        reasoning_effort: Option<ReasoningEffortArg>,
        /// Gemini extended-thinking budget in tokens (Gemini 2.5+).
        /// `0` explicitly disables thinking for this agent. Omit the flag
        /// to leave the current value unchanged. No CLI sentinel to clear
        /// back to "inherit server default" — matches the HTTP API
        /// semantics on `thinking_budget_tokens` (#794).
        #[arg(long)]
        gemini_thinking_budget: Option<u32>,
        /// Per-agent summary-task provider override (#872, #876). Omit
        /// to leave the current value unchanged. Empty string is rejected
        /// — use `--clear-summary-provider` to clear the override (the
        /// pair-only invariant requires unambiguous wire shapes; see
        /// `--clear-summary-provider`). Must be paired with
        /// `--summary-model` so the post-update record satisfies the
        /// pair-only invariant; setting one without the other is rejected.
        #[arg(long)]
        summary_provider: Option<String>,
        /// Per-agent summary-task model override (#872, #876). Omit to
        /// leave the current value unchanged. Empty string is rejected —
        /// use `--clear-summary-model` to clear. Must be paired with
        /// `--summary-provider`.
        #[arg(long)]
        summary_model: Option<String>,
        /// Clear the per-agent `summary_provider` override back to "inherit
        /// server default". Mutually exclusive with `--summary-provider`
        /// (sending both returns `CLEAR_AND_VALUE_CONFLICT`). Should
        /// usually be sent together with `--clear-summary-model` so the
        /// post-update record stays paired (or both already unset).
        #[arg(long)]
        clear_summary_provider: bool,
        /// Clear the per-agent `summary_model` override back to "inherit
        /// server default". Mutually exclusive with `--summary-model`.
        /// Should usually be sent together with
        /// `--clear-summary-provider`.
        #[arg(long)]
        clear_summary_model: bool,
        /// Per-agent git worktree-isolation mode (#946). Flips the
        /// stored value: `off → git` runs `git worktree add` and
        /// pins the agent's sandbox at the new worktree; `git → off`
        /// runs `git worktree remove` and refuses if the worktree
        /// has uncommitted changes (pass `--force-worktree-remove`
        /// to override — that path also deletes the `alms/<name>`
        /// branch). Omit the flag to leave the existing setting
        /// unchanged.
        #[arg(long, value_enum)]
        worktree_mode: Option<WorktreeModeArg>,
        /// When flipping `--worktree-mode off` from `git`, force the
        /// removal even if the worktree has uncommitted changes. Has
        /// no effect on other transitions. Discards the worktree
        /// contents AND deletes the `alms/<name>` branch.
        #[arg(long)]
        force_worktree_remove: bool,
    },
}

pub(crate) fn agent_list(store: &SqliteStore, json: bool) -> anyhow::Result<()> {
    let agents = store.list_agents()?;
    if json {
        println!("{}", serde_json::to_string_pretty(&agents)?);
        return Ok(());
    }
    if agents.is_empty() {
        println!("No agents registered.");
        return Ok(());
    }
    println!(
        "{:<20} {:<10} {:<30} {:<8} LAST ACTIVE",
        "NAME", "ID", "MODEL", "DEFAULT"
    );
    for a in &agents {
        let id_short = short_id(&a.id);
        let model = a.model.as_deref().unwrap_or("(server default)");
        let default = if a.is_default { "*" } else { "" };
        println!(
            "{:<20} {:<10} {:<30} {:<8} {}",
            a.name,
            id_short,
            model,
            default,
            fmt_time(&a.last_active)
        );
    }
    Ok(())
}

/// Options for creating a new agent via the CLI.
pub(crate) struct AgentCreateOpts<'a> {
    pub name: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub posture: Option<String>,
    pub provider: Option<String>,
    /// Anthropic extended-thinking budget override (tokens). `Some(0)`
    /// explicitly disables thinking; `None` inherits the server default.
    /// Matches the HTTP API semantics on `POST /agents`.
    pub thinking_budget_tokens: Option<u32>,
    /// OpenAI-compat reasoning effort override (`low` / `medium` / `high`
    /// / `minimal`). `None` inherits the server default from
    /// `[llm.openai]`. Matches the HTTP API semantics on `POST /agents`.
    pub reasoning_effort: Option<alms_core::config::ReasoningEffort>,
    /// Gemini extended-thinking budget override (tokens). `Some(0)`
    /// explicitly disables thinking; `None` inherits the server default
    /// from `[llm.gemini]`. Matches the HTTP API semantics on
    /// `POST /agents` (#794).
    pub gemini_thinking_budget: Option<u32>,
    /// Per-agent summary-task provider override (#872, #876). `None`
    /// inherits the server-level `[context].summary_provider`. Must be
    /// set together with `summary_model` — partial pairs are rejected
    /// at construction time below to mirror the HTTP API's pair-only
    /// invariant.
    pub summary_provider: Option<String>,
    /// Per-agent summary-task model override (#872, #876). `None`
    /// inherits the server-level `[context].summary_model`. Must be
    /// set together with `summary_provider`.
    pub summary_model: Option<String>,
    /// Per-agent git worktree-isolation mode (#946). `Off` (default)
    /// keeps the sandbox at the project root; `Git` runs
    /// `git worktree add <project>/.alms/worktrees/<name>/ -b
    /// alms/<name>` AT THIS CALL SITE before persisting the agent.
    /// Non-git projects fail with `WORKTREE_REQUIRES_GIT` and the
    /// agent is NOT persisted.
    pub worktree_mode: WorktreeMode,
    /// Optional project root for resolving `git worktree add` when
    /// `worktree_mode == Git`. `None` is fine for `Off` agents and
    /// for unit tests; `Some(path)` is required when `worktree_mode`
    /// is `Git` — the CLI's `agent create` wires this from
    /// `config.server.project_root()`.
    pub project_root: Option<&'a std::path::Path>,
    pub default: bool,
    pub json: bool,
    pub workspace_dir: Option<&'a std::path::Path>,
}

pub(crate) fn agent_create(store: &SqliteStore, opts: AgentCreateOpts<'_>) -> anyhow::Result<()> {
    let AgentCreateOpts {
        name,
        description,
        model,
        posture,
        provider,
        thinking_budget_tokens,
        reasoning_effort,
        gemini_thinking_budget,
        summary_provider,
        summary_model,
        worktree_mode,
        project_root,
        default,
        json,
        workspace_dir,
    } = opts;

    validate_agent_name(&name)?;

    // Worktree-mode (#946). Provision the worktree BEFORE persisting
    // the agent record so a non-git project produces a clean error
    // with NO half-created agent and NO half-created worktree
    // directory. Mirrors the HTTP `POST /agents` shape.
    //
    // The CLI does NOT consult `[security].allow_full_os_access`
    // here — that's a daemon-side runtime concern, and the worktree
    // creation is purely a side-effect of the registry record. The
    // gateway's boot-time WARN catches the overlap on the next
    // restart.
    if worktree_mode == WorktreeMode::Git {
        let project_root = project_root.ok_or_else(|| {
            anyhow::anyhow!(
                "--worktree-mode git requires a project root, but the CLI was invoked \
                 without one. Run `alms agent create` from inside an `alms` daemon \
                 project directory or set ALMS_PROJECT_ROOT explicitly."
            )
        })?;

        // Refuse to create a worktree if the agent name is already
        // taken — same shape as the HTTP path.
        if let Ok(Some(_)) = store.load_agent_by_name(&name) {
            anyhow::bail!("Agent name '{name}' already exists");
        }
        if let Err(e) = alms_core::worktree::create_worktree(project_root, &name) {
            return Err(match e {
                alms_core::worktree::WorktreeError::NotAGitRepo => anyhow::anyhow!(
                    "WORKTREE_REQUIRES_GIT: --worktree-mode git requires the project \
                     root to be a git working tree (project: {}). Either run `git init` \
                     in the project directory or omit --worktree-mode to use the \
                     project-root sandbox.",
                    project_root.display()
                ),
                other => anyhow::anyhow!("WORKTREE_GIT_FAILED: {other}"),
            });
        }
    }

    // Reject empty / whitespace-only summary values up front (S2 follow-up
    // on #876). The HTTP `PUT /agents/{id}` path returns `SUMMARY_FIELD_EMPTY`
    // for these inputs, and `POST /agents` normalizes them to `None` for
    // back-compat. The CLI used to silently let `Some("")` / `Some("  ")`
    // through to the registry — both halves of the pair would pass the
    // pair-check below as `(Some, Some)` and persist as empty strings,
    // diverging from both HTTP shapes. Reject outright instead so all
    // surfaces (CLI + HTTP CRUD) treat empty-as-clear ambiguously and
    // direct operators to omit the flags entirely on `agent create`.
    if matches!(summary_provider.as_deref().map(str::trim), Some("")) {
        anyhow::bail!(
            "--summary-provider cannot be an empty or whitespace-only string. \
             Omit the flag entirely to inherit the server-level \
             [context].summary_provider, or pair it with --summary-model and a \
             non-empty value."
        );
    }
    if matches!(summary_model.as_deref().map(str::trim), Some("")) {
        anyhow::bail!(
            "--summary-model cannot be an empty or whitespace-only string. \
             Omit the flag entirely to inherit the server-level \
             [context].summary_model, or pair it with --summary-provider and a \
             non-empty value."
        );
    }

    // Pair-only invariant for the per-agent summary fields (#872, #876).
    // Mirrors the HTTP `POST /agents` validator — setting one without the
    // other is meaningless and would silently fall through to the
    // server-level `[context].summary_*`. Surface a clear CLI error here
    // rather than letting the user discover the mismatch only when the
    // summary task fires at run time.
    match (summary_provider.as_deref(), summary_model.as_deref()) {
        (Some(_), None) => {
            anyhow::bail!(
                "--summary-provider is set but --summary-model is empty. \
                 Set both flags together — the summary provider's wire model \
                 namespace is independent of the agent's primary provider, so \
                 partial settings cannot be safely resolved."
            );
        }
        (None, Some(_)) => {
            anyhow::bail!(
                "--summary-model is set but --summary-provider is empty. \
                 Set both flags together — leaving --summary-provider unset \
                 would fall through to the server-level [context].summary_provider, \
                 which may not match this model's namespace."
            );
        }
        _ => {}
    }

    let now = chrono::Utc::now();
    let agent = AgentRecord {
        id: AgentId::new(),
        name,
        description: description.unwrap_or_default(),
        model,
        posture,
        provider,
        telegram_token: None,
        thinking_budget_tokens,
        reasoning_effort,
        gemini_thinking_budget,
        // Per-agent summary overrides (#872, #876). The empty /
        // whitespace-only rejection and the pair-only invariant are
        // both enforced above, so at this point both fields are either
        // `Some(non_empty)` together or both `None`. Values reach the
        // registry without further trimming — the HTTP layers (POST
        // back-compat normalize, PUT trim-on-apply) do their own
        // trimming, and a CLI operator who explicitly typed a value
        // with surrounding whitespace probably means it.
        summary_provider,
        summary_model,
        worktree_mode,
        // Debug mode (#1003) is operator-flippable via PATCH /agents/{id}
        // (or the per-agent edit modal in the web UI) — `alms agent
        // create` lands the record with `false` so existing CLI
        // workflows are unaffected. Operators who want it enabled
        // flip it after creation just like every other mutable knob.
        debug_mode: false,
        is_default: default,
        created_at: now,
        last_active: now,
    };

    if let Err(e) = store.create_agent(&agent) {
        if matches!(&e, alms_core::AlmsError::DuplicateName(_)) {
            // Roll back the worktree we just created so a duplicate
            // collision doesn't leave a stranded worktree dir behind.
            if worktree_mode == WorktreeMode::Git
                && let Some(project_root) = project_root
                && let Err(rb) =
                    alms_core::worktree::remove_worktree(project_root, &agent.name, true)
            {
                eprintln!(
                    "Warning: failed to roll back worktree after duplicate-name collision: {rb}"
                );
            }
            anyhow::bail!("Agent name '{}' already exists", agent.name);
        }
        return Err(e.into());
    }

    if default {
        store.set_default_agent(agent.id)?;
    }

    // Create workspace directory and initial files
    if let Some(ws_dir) = workspace_dir {
        let agent_ws_dir = ws_dir.join(&agent.name);
        if let Err(e) = alms_core::init_workspace_files(&agent_ws_dir) {
            eprintln!(
                "Warning: could not create workspace files in {}: {}",
                agent_ws_dir.display(),
                e
            );
        }
    }

    if json {
        let mut val = serde_json::to_value(&agent)?;
        if let Some(ws_dir) = workspace_dir {
            val["workspace_path"] =
                serde_json::Value::String(ws_dir.join(&agent.name).display().to_string());
        }
        println!("{}", serde_json::to_string_pretty(&val)?);
    } else {
        println!("Created agent '{}' ({})", agent.name, agent.id);
        if let Some(ws_dir) = workspace_dir {
            println!("  Workspace: {}", ws_dir.join(&agent.name).display());
        }
    }
    Ok(())
}

pub(crate) fn agent_show(store: &SqliteStore, name_or_id: &str, json: bool) -> anyhow::Result<()> {
    let agent = resolve_agent(store, name_or_id)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&agent)?);
        return Ok(());
    }
    println!("Name:          {}", agent.name);
    println!("ID:            {}", agent.id);
    println!(
        "Description:   {}",
        if agent.description.is_empty() {
            "(none)"
        } else {
            &agent.description
        }
    );
    println!(
        "Model:         {}",
        agent.model.as_deref().unwrap_or("(server default)")
    );
    println!(
        "Posture:       {}",
        agent.posture.as_deref().unwrap_or("(server default)")
    );
    println!(
        "Provider:      {}",
        agent.provider.as_deref().unwrap_or("(server default)")
    );
    // Render the Anthropic extended-thinking budget so operators can verify
    // `--thinking-budget-tokens` landed as expected. `Some(0)` prints as a
    // distinct "disabled (explicit)" so it's visually separable from the
    // inherit-server-default case.
    let thinking_display = match agent.thinking_budget_tokens {
        Some(0) => "disabled (explicit)".to_string(),
        Some(n) => format!("{n} tokens"),
        None => "(server default)".to_string(),
    };
    println!("Thinking:      {thinking_display}");
    // Render the OpenAI-compat reasoning effort so operators can verify
    // `--reasoning-effort` landed as expected (#768).
    let reasoning_display = match agent.reasoning_effort {
        Some(effort) => effort.to_string(),
        None => "(server default)".to_string(),
    };
    println!("Reasoning:     {reasoning_display}");
    // Render the Gemini extended-thinking budget (#794). `Some(0)` prints
    // as a distinct "disabled (explicit)" so it's visually separable from
    // the inherit-server-default case — mirrors the Anthropic Thinking
    // row above.
    let gemini_thinking_display = match agent.gemini_thinking_budget {
        Some(0) => "disabled (explicit)".to_string(),
        Some(n) => format!("{n} tokens"),
        None => "(server default)".to_string(),
    };
    println!("Gemini Think:  {gemini_thinking_display}");
    // Per-agent summary provider/model overrides (#872, #876). Both
    // fields are either Some-together or None-together (the pair-only
    // invariant is enforced at create/PATCH time). `(server default)`
    // means the agent inherits `[context].summary_provider` /
    // `[context].summary_model` from `alms.toml`.
    // Use "Sum Provider:" / "Sum Model:" rather than "Summary Prov:" /
    // "Summary Model:" so the truncation lands on the prefix word rather
    // than the field role — symmetric with how `Gemini Think:` truncates
    // `Gemini Thinking:` above. Reads less oddly than mid-word truncation.
    println!(
        "Sum Provider:  {}",
        agent
            .summary_provider
            .as_deref()
            .unwrap_or("(server default)")
    );
    println!(
        "Sum Model:     {}",
        agent.summary_model.as_deref().unwrap_or("(server default)")
    );
    // Per-agent worktree-isolation mode (#946). Surface the wire
    // string verbatim so operators can match against the same
    // value `agent create --worktree-mode <value>` would accept.
    println!("Worktree:      {}", agent.worktree_mode.as_wire_str());
    println!(
        "Default:       {}",
        if agent.is_default { "yes" } else { "no" }
    );
    println!("Created:       {}", fmt_time(&agent.created_at));
    println!("Last Active:   {}", fmt_time(&agent.last_active));
    Ok(())
}

pub(crate) fn agent_delete(
    store: &SqliteStore,
    name_or_id: &str,
    force: bool,
    json: bool,
    project_root: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let agent = resolve_agent(store, name_or_id)?;
    if agent.is_default && !force {
        anyhow::bail!(
            "Cannot delete the default agent '{}'. Set another agent as default first, or use --force.",
            agent.name
        );
    }
    // Worktree teardown (#946). Runs BEFORE the SQLite delete so an
    // uncommitted-changes refusal surfaces a clean error without
    // orphaning the registry record. `--force` drives both the
    // default-agent override and the uncommitted-changes override
    // because the user's intent is "I really want this gone" and
    // splitting the flag would be more friction than safety.
    if agent.worktree_mode == WorktreeMode::Git {
        let project_root = project_root.ok_or_else(|| {
            anyhow::anyhow!(
                "Agent '{}' has worktree_mode=git but no project root was provided. \
                 Run from inside the project directory or set ALMS_PROJECT_ROOT.",
                agent.name
            )
        })?;
        if let Err(e) = alms_core::worktree::remove_worktree(project_root, &agent.name, force) {
            return Err(match e {
                alms_core::worktree::WorktreeError::UncommittedChanges => anyhow::anyhow!(
                    "WORKTREE_HAS_UNCOMMITTED_CHANGES: agent '{}' has uncommitted changes \
                     in its worktree. Commit / stash them, or pass --force to delete \
                     the agent — this discards the worktree contents AND deletes the \
                     `alms/{}` branch.",
                    agent.name,
                    agent.name
                ),
                other => anyhow::anyhow!("WORKTREE_GIT_FAILED: {other}"),
            });
        }
    }
    store.delete_agent(agent.id)?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "deleted": agent.name })
        );
    } else {
        println!("Deleted agent '{}'", agent.name);
    }
    Ok(())
}

pub(crate) fn agent_set_default(
    store: &SqliteStore,
    name_or_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let agent = resolve_agent(store, name_or_id)?;
    store.set_default_agent(agent.id)?;
    if json {
        println!(
            "{}",
            serde_json::json!({ "ok": true, "default_agent": agent.name })
        );
    } else {
        println!("Set '{}' as default agent", agent.name);
    }
    Ok(())
}

/// Options for updating an agent's configuration via the CLI.
pub(crate) struct AgentConfigOpts<'a> {
    pub name_or_id: &'a str,
    pub model: Option<String>,
    pub posture: Option<String>,
    pub provider: Option<String>,
    pub description: Option<String>,
    /// Anthropic extended-thinking budget override (tokens). `Some(0)`
    /// explicitly disables thinking; `Some(n)` sets the budget. `None`
    /// here means "leave unchanged" — mirrors the HTTP `PATCH /agents`
    /// path where a missing field is a no-op.
    pub thinking_budget_tokens: Option<u32>,
    /// OpenAI-compat reasoning effort override (`low`/`medium`/`high`/
    /// `minimal`). `None` leaves the stored value unchanged. Matches
    /// the HTTP `PATCH /agents` semantics on `reasoning_effort` (#768).
    pub reasoning_effort: Option<alms_core::config::ReasoningEffort>,
    /// Gemini extended-thinking budget override (tokens). `Some(0)`
    /// explicitly disables thinking; `Some(n)` sets the budget. `None`
    /// leaves the stored value unchanged — matches the HTTP `PATCH
    /// /agents` semantics on `gemini_thinking_budget` (#794).
    pub gemini_thinking_budget: Option<u32>,
    /// Per-agent summary-task provider override (#872, #876).
    /// `Some(value)` writes through; `None` leaves the stored value
    /// unchanged. Empty string is rejected (use `clear_summary_provider`
    /// instead). Mutually exclusive with `clear_summary_provider: true`.
    pub summary_provider: Option<String>,
    /// Per-agent summary-task model override (#872, #876). Same shape
    /// as `summary_provider` above.
    pub summary_model: Option<String>,
    /// When `true`, clear the per-agent `summary_provider` override
    /// back to `None` (inherit server default). Mutually exclusive
    /// with `summary_provider: Some(_)` — passing both returns
    /// `CLEAR_AND_VALUE_CONFLICT`. Should usually be paired with
    /// `clear_summary_model: true` so the post-update record stays
    /// paired (or both `None`).
    pub clear_summary_provider: bool,
    /// When `true`, clear the per-agent `summary_model` override.
    /// Mutually exclusive with `summary_model: Some(_)`.
    pub clear_summary_model: bool,
    /// Per-agent git worktree-isolation mode (#946). `Some(mode)`
    /// flips the stored value AND triggers the matching git
    /// worktree side-effect (`add` / `remove`). `None` leaves the
    /// stored value unchanged.
    pub worktree_mode: Option<WorktreeMode>,
    /// When flipping `Git → Off`, force the worktree removal even
    /// if it has uncommitted changes. Has no effect on other
    /// transitions. Discards the worktree contents AND deletes the
    /// `alms/<name>` branch.
    pub force_worktree_remove: bool,
    /// Project root for resolving `git worktree add` / `remove`
    /// when `worktree_mode` is set. `None` is fine for transitions
    /// that don't run git (e.g. PATCH that omits worktree_mode).
    pub project_root: Option<&'a std::path::Path>,
    pub json: bool,
}

pub(crate) fn agent_config(store: &SqliteStore, opts: AgentConfigOpts<'_>) -> anyhow::Result<()> {
    let AgentConfigOpts {
        name_or_id,
        model,
        posture,
        provider,
        description,
        thinking_budget_tokens,
        reasoning_effort,
        gemini_thinking_budget,
        summary_provider,
        summary_model,
        clear_summary_provider,
        clear_summary_model,
        worktree_mode,
        force_worktree_remove,
        project_root,
        json,
    } = opts;

    // CLEAR_AND_VALUE_CONFLICT for the summary fields (#872, #876).
    // Mirrors the HTTP `PATCH /agents/{id}` semantics — sending both a
    // value and a clear flag for the same field is ambiguous and
    // rejected. Surface the same error code at the CLI layer so users
    // who scripted against the HTTP path see consistent diagnostics.
    if summary_provider.is_some() && clear_summary_provider {
        anyhow::bail!(
            "CLEAR_AND_VALUE_CONFLICT: cannot send both --summary-provider \
             and --clear-summary-provider in the same command"
        );
    }
    if summary_model.is_some() && clear_summary_model {
        anyhow::bail!(
            "CLEAR_AND_VALUE_CONFLICT: cannot send both --summary-model \
             and --clear-summary-model in the same command"
        );
    }

    // Reject empty / whitespace-only summary values up front. The HTTP
    // `PUT /agents/{id}` path returns `SUMMARY_FIELD_EMPTY` after
    // trimming, because empty-string-as-clear would conflict with the
    // explicit `clear_*` sentinel; mirror that here for parity (and
    // also reject pure whitespace, which would trim to empty server-
    // side anyway).
    if matches!(summary_provider.as_deref().map(str::trim), Some("")) {
        anyhow::bail!(
            "--summary-provider cannot be an empty or whitespace-only string \
             — use --clear-summary-provider to reset back to inheriting the \
             server-level [context].summary_provider"
        );
    }
    if matches!(summary_model.as_deref().map(str::trim), Some("")) {
        anyhow::bail!(
            "--summary-model cannot be an empty or whitespace-only string \
             — use --clear-summary-model to reset"
        );
    }

    let mut agent = resolve_agent(store, name_or_id)?;

    if let Some(d) = description {
        agent.description = d;
    }
    if let Some(m) = model {
        agent.model = if m.is_empty() { None } else { Some(m) };
    }
    if let Some(p) = posture {
        agent.posture = if p.is_empty() { None } else { Some(p) };
    }
    if let Some(prov) = provider {
        agent.provider = if prov.is_empty() { None } else { Some(prov) };
    }
    // `Some(n)` (including `Some(0)`) is an explicit override matching the
    // HTTP API's `PUT /agents/{id}` semantics. There's no CLI sentinel to
    // clear back to "inherit server default" today — per Tim S3 on #775,
    // we defer that until someone actually hits the trap.
    if let Some(budget) = thinking_budget_tokens {
        agent.thinking_budget_tokens = Some(budget);
    }

    // Same shape as `thinking_budget_tokens`: `Some(effort)` is an explicit
    // per-agent override; `None` leaves the stored value unchanged (#768).
    if let Some(effort) = reasoning_effort {
        agent.reasoning_effort = Some(effort);
    }

    // Gemini thinking budget: `Some(n)` (including `Some(0)`) is an
    // explicit override; `None` leaves the stored value unchanged. No
    // CLI sentinel to clear back to "inherit server default" — matches
    // the HTTP `PUT /agents/{id}` semantics (#794).
    if let Some(budget) = gemini_thinking_budget {
        agent.gemini_thinking_budget = Some(budget);
    }

    // Per-agent summary fields (#872, #876). The clear flags reset to
    // `None`; non-empty values write through (trimmed to mirror the
    // HTTP layer's `apply_update_request` behaviour). The pair-only
    // invariant is enforced AFTER applying both halves so a single
    // command setting both fields stays valid even when the
    // pre-existing record was already paired.
    if clear_summary_provider {
        agent.summary_provider = None;
    } else if let Some(provider) = summary_provider {
        agent.summary_provider = Some(provider.trim().to_string());
    }
    if clear_summary_model {
        agent.summary_model = None;
    } else if let Some(model) = summary_model {
        agent.summary_model = Some(model.trim().to_string());
    }

    // Per-agent worktree-mode flip (#946). The side-effecting `git
    // worktree add` / `remove` runs BEFORE the SQLite update so a
    // failure surfaces a clean error without leaving the record
    // half-changed. Mirrors the HTTP `PATCH /agents/{id}` flow.
    let prev_worktree_mode = agent.worktree_mode;
    if let Some(new_mode) = worktree_mode {
        agent.worktree_mode = new_mode;
        if prev_worktree_mode != new_mode {
            let project_root = project_root.ok_or_else(|| {
                anyhow::anyhow!(
                    "--worktree-mode flip requires a project root, but the CLI was \
                     invoked without one. Run from inside the project directory or set \
                     ALMS_PROJECT_ROOT explicitly."
                )
            })?;
            match (prev_worktree_mode, new_mode) {
                (WorktreeMode::Off, WorktreeMode::Git) => {
                    if let Err(e) = alms_core::worktree::create_worktree(project_root, &agent.name)
                    {
                        return Err(match e {
                            alms_core::worktree::WorktreeError::NotAGitRepo => anyhow::anyhow!(
                                "WORKTREE_REQUIRES_GIT: --worktree-mode git requires the project \
                                 root to be a git working tree (project: {}).",
                                project_root.display()
                            ),
                            other => anyhow::anyhow!("WORKTREE_GIT_FAILED: {other}"),
                        });
                    }
                }
                (WorktreeMode::Git, WorktreeMode::Off) => {
                    if let Err(e) = alms_core::worktree::remove_worktree(
                        project_root,
                        &agent.name,
                        force_worktree_remove,
                    ) {
                        return Err(match e {
                            alms_core::worktree::WorktreeError::UncommittedChanges => {
                                anyhow::anyhow!(
                                    "WORKTREE_HAS_UNCOMMITTED_CHANGES: agent '{}' has \
                                     uncommitted changes in its worktree. Commit / stash \
                                     them or pass --force-worktree-remove to discard — \
                                     this drops the worktree contents AND deletes the \
                                     `alms/{}` branch.",
                                    agent.name,
                                    agent.name
                                )
                            }
                            other => anyhow::anyhow!("WORKTREE_GIT_FAILED: {other}"),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    // Pair-only invariant on the post-update record state. Mirrors the
    // HTTP `PUT /agents/{id}` validator — setting one field without the
    // other (whether by patching just one half or by clearing one half
    // when the other is already set) is rejected. Note the HTTP layer
    // ALSO validates the provider against `[llm.providers.<name>]` and
    // the secrets store; the CLI cannot do that without spinning up
    // AppState, so we leave that check to the daemon at run time —
    // matches the contract Atlas described (load-time vs run-time
    // layers in #877/#878).
    match (
        agent.summary_provider.as_deref(),
        agent.summary_model.as_deref(),
    ) {
        (Some(_), None) => {
            anyhow::bail!(
                "SUMMARY_PROVIDER_REQUIRES_MODEL: agent.summary_provider would \
                 be set but agent.summary_model is empty after this update. \
                 Pass --summary-model together with --summary-provider, or \
                 add --clear-summary-provider to drop the existing override."
            );
        }
        (None, Some(_)) => {
            anyhow::bail!(
                "SUMMARY_MODEL_REQUIRES_PROVIDER: agent.summary_model would be \
                 set but agent.summary_provider is empty after this update. \
                 Pass --summary-provider together with --summary-model, or \
                 add --clear-summary-model to drop the existing override."
            );
        }
        _ => {}
    }

    agent.last_active = chrono::Utc::now();
    store.update_agent(&agent)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&agent)?);
    } else {
        println!("Updated agent '{}'", agent.name);
        agent_show(store, &agent.name, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helpers::{make_agent, new_store, resolve_agent};

    #[test]
    fn test_create_list_show_delete_roundtrip() {
        let store = new_store();

        // Create
        agent_create(
            &store,
            AgentCreateOpts {
                name: "test-agent".into(),
                description: Some("desc".into()),
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();

        // List
        let agents = store.list_agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].name, "test-agent");
        assert_eq!(agents[0].description, "desc");

        // Show (just verifying resolve works)
        let agent = resolve_agent(&store, "test-agent").unwrap();
        assert_eq!(agent.description, "desc");

        // Delete
        agent_delete(&store, "test-agent", false, false, None).unwrap();
        assert!(store.list_agents().unwrap().is_empty());
    }

    #[test]
    fn test_create_duplicate_name_fails() {
        let store = new_store();
        agent_create(
            &store,
            AgentCreateOpts {
                name: "dup".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();
        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "dup".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    #[test]
    fn test_create_invalid_name_fails() {
        let store = new_store();
        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "Bad Name".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("lowercase"));
    }

    #[test]
    fn test_create_creates_workspace_dir() {
        let store = new_store();
        let tmp = tempfile::TempDir::new().unwrap();
        let ws_dir = tmp.path().join("workspace");

        agent_create(
            &store,
            AgentCreateOpts {
                name: "reviewer".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: Some(&ws_dir),
            },
        )
        .unwrap();

        // Workspace directory and files should exist at {workspace_dir}/{name}/
        let agent_dir = ws_dir.join("reviewer");
        assert!(agent_dir.is_dir());
        for filename in alms_core::WORKSPACE_FILENAMES {
            assert!(
                agent_dir.join(filename).exists(),
                "Expected workspace file {filename} to exist"
            );
        }
    }

    #[test]
    fn test_set_default() {
        let store = new_store();
        make_agent(&store, "alpha");
        make_agent(&store, "beta");

        agent_set_default(&store, "beta", false).unwrap();
        let default = store.get_default_agent().unwrap().unwrap();
        assert_eq!(default.name, "beta");

        // Switch default
        agent_set_default(&store, "alpha", false).unwrap();
        let default = store.get_default_agent().unwrap().unwrap();
        assert_eq!(default.name, "alpha");

        // Verify beta is no longer default
        let beta = resolve_agent(&store, "beta").unwrap();
        assert!(!beta.is_default);
    }

    #[test]
    fn test_delete_default_blocked() {
        let store = new_store();
        let agent = make_agent(&store, "main");
        store.set_default_agent(agent.id).unwrap();

        let err = agent_delete(&store, "main", false, false, None).unwrap_err();
        assert!(err.to_string().contains("Cannot delete the default"));
    }

    #[test]
    fn test_delete_default_with_force() {
        let store = new_store();
        let agent = make_agent(&store, "main");
        store.set_default_agent(agent.id).unwrap();

        agent_delete(&store, "main", true, false, None).unwrap();
        assert!(store.list_agents().unwrap().is_empty());
    }

    #[test]
    fn test_config_update() {
        let store = new_store();
        make_agent(&store, "configurable");

        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "configurable",
                model: Some("new-model".into()),
                posture: Some("guarded".into()),
                provider: None,
                description: Some("updated desc".into()),
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap();

        let agent = resolve_agent(&store, "configurable").unwrap();
        assert_eq!(agent.model.as_deref(), Some("new-model"));
        assert_eq!(agent.posture.as_deref(), Some("guarded"));
        assert_eq!(agent.description, "updated desc");
    }

    #[test]
    fn test_config_clear_override() {
        let store = new_store();
        let now = chrono::Utc::now();
        let agent = AgentRecord {
            id: AgentId::new(),
            name: "clearable".to_string(),
            description: String::new(),
            model: Some("old-model".to_string()),
            posture: Some("guarded".to_string()),
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: None,
            summary_model: None,
            worktree_mode: WorktreeMode::Off,
            debug_mode: false,
            is_default: false,
            created_at: now,
            last_active: now,
        };
        store.create_agent(&agent).unwrap();

        // Clear model by passing empty string
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "clearable",
                model: Some(String::new()),
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap();
        let updated = resolve_agent(&store, "clearable").unwrap();
        assert!(updated.model.is_none());
        // Posture untouched
        assert_eq!(updated.posture.as_deref(), Some("guarded"));
    }

    // -- CLI --thinking-budget-tokens flag on create + update (PR #775 S5) ------
    //
    // `alms agent create --thinking-budget-tokens N` must persist the value
    // verbatim (including `Some(0)` as an explicit per-agent disable).
    // `alms agent config --thinking-budget-tokens N` must overwrite an
    // existing value, mirroring the HTTP API's `PUT /agents/{id}` behaviour.
    #[test]
    fn test_create_with_thinking_budget() {
        let store = new_store();
        agent_create(
            &store,
            AgentCreateOpts {
                name: "thinker".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: Some(8192),
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();
        let agent = resolve_agent(&store, "thinker").unwrap();
        assert_eq!(agent.thinking_budget_tokens, Some(8192));
    }

    #[test]
    fn test_create_with_thinking_budget_zero_disables() {
        // `--thinking-budget-tokens 0` is an explicit per-agent opt-out, not
        // "inherit server default". Must survive round-trip as `Some(0)`.
        let store = new_store();
        agent_create(
            &store,
            AgentCreateOpts {
                name: "no-thinker".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: Some(0),
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();
        let agent = resolve_agent(&store, "no-thinker").unwrap();
        assert_eq!(agent.thinking_budget_tokens, Some(0));
    }

    #[test]
    fn test_config_updates_thinking_budget() {
        let store = new_store();
        make_agent(&store, "tuner");

        // Set an initial value.
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "tuner",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: Some(4096),
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap();
        let after_set = resolve_agent(&store, "tuner").unwrap();
        assert_eq!(after_set.thinking_budget_tokens, Some(4096));

        // Overwrite with an explicit disable (`Some(0)`).
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "tuner",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: Some(0),
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap();
        let after_zero = resolve_agent(&store, "tuner").unwrap();
        assert_eq!(after_zero.thinking_budget_tokens, Some(0));

        // Omitting the flag (`None`) must leave the stored value untouched.
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "tuner",
                model: Some("some-model".into()),
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap();
        let after_noop = resolve_agent(&store, "tuner").unwrap();
        assert_eq!(
            after_noop.thinking_budget_tokens,
            Some(0),
            "omitted flag must not clobber existing thinking_budget_tokens"
        );
        assert_eq!(after_noop.model.as_deref(), Some("some-model"));
    }

    // -- CLI --gemini-thinking-budget flag on create + update (#794) ----------
    //
    // Mirrors the `thinking_budget_tokens` CLI tests above. `alms agent
    // create --gemini-thinking-budget N` must persist the value verbatim
    // (including `Some(0)` as an explicit per-agent disable). `alms agent
    // config --gemini-thinking-budget N` must overwrite; omitting the
    // flag must leave the stored value untouched.

    #[test]
    fn test_create_with_gemini_thinking_budget() {
        let store = new_store();
        agent_create(
            &store,
            AgentCreateOpts {
                name: "gemini-thinker".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: Some(16384),
                summary_provider: None,
                summary_model: None,
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();
        let agent = resolve_agent(&store, "gemini-thinker").unwrap();
        assert_eq!(agent.gemini_thinking_budget, Some(16384));
    }

    #[test]
    fn test_create_with_gemini_thinking_budget_zero_disables() {
        // `--gemini-thinking-budget 0` is an explicit per-agent opt-out,
        // not "inherit server default". Must survive round-trip as
        // `Some(0)`.
        let store = new_store();
        agent_create(
            &store,
            AgentCreateOpts {
                name: "no-gemini-thinker".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: Some(0),
                summary_provider: None,
                summary_model: None,
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();
        let agent = resolve_agent(&store, "no-gemini-thinker").unwrap();
        assert_eq!(agent.gemini_thinking_budget, Some(0));
    }

    #[test]
    fn test_config_updates_gemini_thinking_budget() {
        let store = new_store();
        make_agent(&store, "gemini-tuner");

        // Set an initial value.
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "gemini-tuner",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: Some(4096),
                summary_provider: None,
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap();
        let after_set = resolve_agent(&store, "gemini-tuner").unwrap();
        assert_eq!(after_set.gemini_thinking_budget, Some(4096));

        // Overwrite with an explicit disable (`Some(0)`).
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "gemini-tuner",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: Some(0),
                summary_provider: None,
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap();
        let after_zero = resolve_agent(&store, "gemini-tuner").unwrap();
        assert_eq!(after_zero.gemini_thinking_budget, Some(0));

        // Omitting the flag (`None`) must leave the stored value
        // untouched — same no-op semantics as `thinking_budget_tokens`.
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "gemini-tuner",
                model: Some("some-model".into()),
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap();
        let after_noop = resolve_agent(&store, "gemini-tuner").unwrap();
        assert_eq!(
            after_noop.gemini_thinking_budget,
            Some(0),
            "omitted flag must not clobber existing gemini_thinking_budget"
        );
        assert_eq!(after_noop.model.as_deref(), Some("some-model"));
    }

    // -- CLI --summary-provider / --summary-model on create + config (#876) ---
    //
    // Mirrors the shape of `--thinking-budget-tokens` and
    // `--gemini-thinking-budget` above. The pair-only invariant
    // (`SUMMARY_PROVIDER_REQUIRES_MODEL` / `SUMMARY_MODEL_REQUIRES_PROVIDER`)
    // and the `--clear-*` sentinel mirror the HTTP API's semantics on
    // `POST /agents` and `PUT /agents/{id}` so users with scripts driven
    // by both surfaces see consistent diagnostics.

    #[test]
    fn test_create_with_summary_provider_and_model_together() {
        let store = new_store();
        agent_create(
            &store,
            AgentCreateOpts {
                name: "summary-set".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some("openrouter".into()),
                summary_model: Some("minimax/minimax-m2.7".into()),
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap();
        let agent = resolve_agent(&store, "summary-set").unwrap();
        assert_eq!(agent.summary_provider.as_deref(), Some("openrouter"));
        assert_eq!(agent.summary_model.as_deref(), Some("minimax/minimax-m2.7"));
    }

    #[test]
    fn test_create_rejects_summary_provider_without_model() {
        // Pair-only invariant — surface the same error code at the CLI
        // layer that the HTTP `POST /agents` validator uses, so users who
        // scripted against either surface see consistent diagnostics.
        let store = new_store();
        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "partial-pair-a".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some("openrouter".into()),
                summary_model: None,
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("--summary-model is empty"),
            "expected partial-pair error, got: {err}"
        );
    }

    #[test]
    fn test_create_rejects_summary_model_without_provider() {
        let store = new_store();
        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "partial-pair-b".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: Some("minimax/minimax-m2.7".into()),
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("--summary-provider is empty"),
            "expected partial-pair error, got: {err}"
        );
    }

    #[test]
    fn test_config_sets_summary_provider_and_model_together() {
        let store = new_store();
        make_agent(&store, "summary-config");
        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "summary-config",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some("openrouter".into()),
                summary_model: Some("minimax/minimax-m2.7".into()),
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap();
        let agent = resolve_agent(&store, "summary-config").unwrap();
        assert_eq!(agent.summary_provider.as_deref(), Some("openrouter"));
        assert_eq!(agent.summary_model.as_deref(), Some("minimax/minimax-m2.7"));
    }

    #[test]
    fn test_config_clear_summary_provider_and_model_together() {
        // Seed an agent that already has both fields set, then clear
        // both via the dedicated sentinels — same pattern as the HTTP
        // API's `clear_summary_provider` / `clear_summary_model`.
        let store = new_store();
        let now = chrono::Utc::now();
        let agent = AgentRecord {
            id: AgentId::new(),
            name: "summary-clear".to_string(),
            description: String::new(),
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: Some("openrouter".to_string()),
            summary_model: Some("minimax/minimax-m2.7".to_string()),
            worktree_mode: WorktreeMode::Off,
            debug_mode: false,
            is_default: false,
            created_at: now,
            last_active: now,
        };
        store.create_agent(&agent).unwrap();

        agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "summary-clear",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                clear_summary_provider: true,
                clear_summary_model: true,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap();
        let updated = resolve_agent(&store, "summary-clear").unwrap();
        assert!(updated.summary_provider.is_none());
        assert!(updated.summary_model.is_none());
    }

    #[test]
    fn test_config_rejects_clear_and_value_together_summary_provider() {
        let store = new_store();
        make_agent(&store, "conflict-a");
        let err = agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "conflict-a",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some("openrouter".into()),
                summary_model: None,
                clear_summary_provider: true,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("CLEAR_AND_VALUE_CONFLICT"),
            "expected CLEAR_AND_VALUE_CONFLICT, got: {err}"
        );
    }

    #[test]
    fn test_config_rejects_clear_and_value_together_summary_model() {
        let store = new_store();
        make_agent(&store, "conflict-b");
        let err = agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "conflict-b",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: Some("minimax/minimax-m2.7".into()),
                clear_summary_provider: false,
                clear_summary_model: true,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("CLEAR_AND_VALUE_CONFLICT"),
            "expected CLEAR_AND_VALUE_CONFLICT, got: {err}"
        );
    }

    #[test]
    fn test_config_rejects_setting_only_summary_provider() {
        // Pair-only invariant on the post-update record state — when the
        // pre-existing record has both fields None, setting only the
        // provider would leave the record asymmetric. Reject with the
        // same error code the HTTP `PUT /agents/{id}` validator uses.
        let store = new_store();
        make_agent(&store, "asymmetric-a");
        let err = agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "asymmetric-a",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some("openrouter".into()),
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("SUMMARY_PROVIDER_REQUIRES_MODEL"),
            "expected SUMMARY_PROVIDER_REQUIRES_MODEL, got: {err}"
        );
    }

    #[test]
    fn test_config_rejects_clearing_only_one_half_of_pair() {
        // Pre-state: both summary fields are set. Clearing only the
        // model would leave provider stranded — same shape the HTTP
        // PATCH layer rejects.
        let store = new_store();
        let now = chrono::Utc::now();
        let agent = AgentRecord {
            id: AgentId::new(),
            name: "asymmetric-b".to_string(),
            description: String::new(),
            model: None,
            posture: None,
            provider: None,
            telegram_token: None,
            thinking_budget_tokens: None,
            reasoning_effort: None,
            gemini_thinking_budget: None,
            summary_provider: Some("openrouter".to_string()),
            summary_model: Some("minimax/minimax-m2.7".to_string()),
            worktree_mode: WorktreeMode::Off,
            debug_mode: false,
            is_default: false,
            created_at: now,
            last_active: now,
        };
        store.create_agent(&agent).unwrap();

        let err = agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "asymmetric-b",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: true,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("SUMMARY_PROVIDER_REQUIRES_MODEL"),
            "expected SUMMARY_PROVIDER_REQUIRES_MODEL, got: {err}"
        );
    }

    #[test]
    fn test_config_rejects_empty_string_summary_fields() {
        // The HTTP layer rejects empty-string for these fields with
        // `SUMMARY_FIELD_EMPTY`; mirror that at the CLI layer so users
        // who type `--summary-provider ""` get a clear error rather
        // than a silently-stored empty record. (Empty-string-as-clear
        // would conflict with the explicit `--clear-*` sentinel.)
        let store = new_store();
        make_agent(&store, "empty-string");
        let err = agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "empty-string",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some(String::new()),
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot be an empty or whitespace-only string"),
            "expected empty-string rejection, got: {err}"
        );
        assert!(
            err.to_string().contains("--summary-provider"),
            "expected --summary-provider in error, got: {err}"
        );
    }

    #[test]
    fn test_config_rejects_empty_string_summary_model() {
        // Symmetric coverage for the `--summary-model` half of the pair
        // — Tim flagged that the existing empty-string test only covered
        // `--summary-provider`. The two checks live on independent code
        // paths, so verify both fire.
        let store = new_store();
        make_agent(&store, "empty-model");
        let err = agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "empty-model",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: Some(String::new()),
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot be an empty or whitespace-only string"),
            "expected empty-string rejection, got: {err}"
        );
        assert!(
            err.to_string().contains("--summary-model"),
            "expected --summary-model in error, got: {err}"
        );
    }

    #[test]
    fn test_config_rejects_whitespace_only_summary_fields() {
        // Whitespace-only inputs trim to empty, which would conflict
        // with the explicit `--clear-*` sentinel just like a literal
        // empty string. Reject both cases on both halves of the pair.
        let store = new_store();
        make_agent(&store, "ws-provider");
        let err = agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "ws-provider",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some("   ".into()),
                summary_model: None,
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot be an empty or whitespace-only string"),
            "expected whitespace-rejection on --summary-provider, got: {err}"
        );

        make_agent(&store, "ws-model");
        let err = agent_config(
            &store,
            AgentConfigOpts {
                name_or_id: "ws-model",
                model: None,
                posture: None,
                provider: None,
                description: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: None,
                summary_model: Some("\t  \n".into()),
                clear_summary_provider: false,
                clear_summary_model: false,
                worktree_mode: None,
                force_worktree_remove: false,
                project_root: None,
                json: false,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot be an empty or whitespace-only string"),
            "expected whitespace-rejection on --summary-model, got: {err}"
        );
    }

    #[test]
    fn test_create_rejects_empty_string_summary_provider() {
        // S2 follow-up on #876 (Tim PR #923 review). Previously
        // `agent_create` let `Some("")` slip through the pair-check as
        // `(Some, Some)` and persisted the empty string verbatim,
        // diverging from the HTTP `POST /agents` back-compat
        // normalization and the `PUT /agents/{id}` `SUMMARY_FIELD_EMPTY`
        // rejection. The CLI now matches PUT's stricter behaviour for
        // both halves of the pair.
        let store = new_store();
        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "create-empty-prov".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some(String::new()),
                summary_model: Some("minimax/minimax-m2.7".into()),
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot be an empty or whitespace-only string"),
            "expected empty-string rejection, got: {err}"
        );
        assert!(
            err.to_string().contains("--summary-provider"),
            "expected --summary-provider in error, got: {err}"
        );
    }

    #[test]
    fn test_create_rejects_empty_string_summary_model() {
        // Symmetric coverage for the `--summary-model` half on `agent
        // create`. See `test_create_rejects_empty_string_summary_provider`.
        let store = new_store();
        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "create-empty-model".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some("openrouter".into()),
                summary_model: Some(String::new()),
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot be an empty or whitespace-only string"),
            "expected empty-string rejection, got: {err}"
        );
        assert!(
            err.to_string().contains("--summary-model"),
            "expected --summary-model in error, got: {err}"
        );
    }

    #[test]
    fn test_create_rejects_whitespace_only_summary_fields() {
        // Whitespace-only inputs trim to empty server-side; reject up
        // front rather than persisting whitespace verbatim. Covers
        // both halves of the pair for parity with `agent_config`.
        let store = new_store();
        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "create-ws-prov".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some("   ".into()),
                summary_model: Some("minimax/minimax-m2.7".into()),
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot be an empty or whitespace-only string"),
            "expected whitespace-rejection on --summary-provider, got: {err}"
        );

        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "create-ws-model".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some("openrouter".into()),
                summary_model: Some("\t  \n".into()),
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot be an empty or whitespace-only string"),
            "expected whitespace-rejection on --summary-model, got: {err}"
        );
    }

    #[test]
    fn test_create_rejects_both_empty_summary_fields() {
        // Regression-pin for the exact scenario Tim flagged in S2:
        // `--summary-provider "" --summary-model ""` should be rejected,
        // not pass the pair-check as `(Some, Some)` and persist a
        // double-empty record.
        let store = new_store();
        let err = agent_create(
            &store,
            AgentCreateOpts {
                name: "create-both-empty".into(),
                description: None,
                model: None,
                posture: None,
                provider: None,
                thinking_budget_tokens: None,
                reasoning_effort: None,
                gemini_thinking_budget: None,
                summary_provider: Some(String::new()),
                summary_model: Some(String::new()),
                worktree_mode: WorktreeMode::Off,
                project_root: None,
                default: false,
                json: false,
                workspace_dir: None,
            },
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot be an empty or whitespace-only string"),
            "expected empty-string rejection, got: {err}"
        );
        // Verify nothing was persisted.
        assert!(resolve_agent(&store, "create-both-empty").is_err());
    }
}
