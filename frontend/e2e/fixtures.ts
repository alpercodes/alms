export function settingsFixture(agentId: string) {
  return {
    version: "0.2.4",
    provider: "openai",
    model: "gpt-5",
    posture: "guarded",
    base_url: "https://api.openai.com/v1",
    stream_chunk_timeout_secs: 30,
    llm_providers: ["openai"],
    agents: [
      {
        id: agentId,
        name: "atlas",
        is_default: true,
        model: null,
        needs_bootstrap: false,
        has_telegram: false,
        worktree_mode: "off",
        debug_mode: false,
        created_at: "2026-07-12T10:00:00Z",
        last_active: "2026-07-12T10:00:00Z",
      },
    ],
    context: {
      strategy: "sliding_summary",
      max_input_tokens: 100_000,
      compact_trigger_pct: 0.8,
      compact_retain_pct: 0.5,
      summary_model: null,
      summary_provider: null,
    },
    session: {
      max_messages: 1_000,
      max_context_tokens: 100_000,
      idle_timeout_secs: 3_600,
      auto_archive: false,
      archive_ttl_secs: 86_400,
    },
    tools: {
      sandbox_root: ".alms/sandbox",
      shell_policy: "guarded",
      timeout_secs: 30,
      max_output_bytes: 30_000,
      enabled: [],
    },
    logging: {
      file_enabled: false,
      file_level: "info",
      rotation: "daily",
      log_dir: null,
    },
    llm: {
      anthropic: {
        thinking_budget_tokens: 0,
        prompt_cache_enabled: true,
      },
      openai: { reasoning_effort: null },
      gemini: {
        thinking_budget: null,
        cache_enabled: false,
        cache_ttl_seconds: 300,
      },
    },
  };
}
