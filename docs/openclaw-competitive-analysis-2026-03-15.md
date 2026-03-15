# OpenClaw Competitive Analysis (2026-03-15)

Snapshot of where OpenClaw stands as of mid-March 2026, and what it means for ALMS.

---

## OpenClaw by the numbers

- **314k GitHub stars** (surpassed React's all-time record in ~60 days)
- **14k open issues**
- **Daily releases** (v2026.3.13 released Mar 14, v2026.3.12 on Mar 13, etc.)
- **TypeScript / Node.js**
- **Formerly known as**: Clawdbot → Moltbot → OpenClaw
- **License**: MIT
- **Platforms**: Desktop, iOS, Android, Docker, Kubernetes, Cloudflare Workers

## Recent features (last week of activity)

- Dashboard v2 with modular views and command palette
- GPT-5.4 + Claude fast mode support
- Provider plugin architecture for Ollama/vLLM/local models
- Chrome DevTools MCP integration with batched browser actions
- iOS/Android native apps with onboarding redesign
- Kubernetes deployment docs
- Multimodal memory (image/audio indexing via Gemini embeddings)
- Security fixes (WebSocket origin validation, Telegram SSRF)
- ClawHub skill marketplace (community-extensible)

## Known problems

- **ClawHub supply-chain attack**: 341 malicious skills found on the marketplace delivering AMOS malware. 21,639 exposed instances discovered by Censys — a 21x increase in one week. This is a fundamental trust problem with their plugin model.
- **14k open issues**: The pace of shipping comes with significant quality debt.
- **Config/session reliability**: Historical pain points (confusing settings, broken context management, flaky sessions) — unclear if these have been resolved under the hood or just papered over with new features.
- **Node.js resource footprint**: V8 runtime overhead is inherent. Relevant for the 24/7 self-hosted VPS use case.

## What this means for ALMS

### Competing on breadth is a losing game

OpenClaw has mobile apps, browser automation, a skill marketplace, multi-provider plugins, Kubernetes support, and a massive contributor base shipping daily. ALMS cannot match this surface area.

### Potential ALMS differentiators

1. **Security by architecture** — ALMS uses capability-gated WASM sandbox for tool isolation. OpenClaw's ClawHub incident shows the risk of an open plugin marketplace without proper sandboxing. ALMS's approach is fundamentally more secure.

2. **Resource efficiency** — Rust single-binary daemon vs Node.js. For the target user running a personal agent 24/7 on a cheap VPS, the difference in idle memory (~5–15 MB vs ~50–80 MB), startup time, and disk footprint could mean a cheaper box is sufficient. (See `docs/tech-stack.md` §15 — needs benchmarks to prove.)

3. **Reliability over velocity** — "Correct and reliable" vs "fast and big" is a viable niche, especially for users who tried OpenClaw and hit config confusion, session bugs, or security concerns.

4. **Multi-agent hierarchy as a core primitive** — ALMS's pure hierarchy (any agent spawns subagents, results flow up, persistent sessions across invocations) is a first-class design, not bolted on.

### Open question

Is there a specific use case where ALMS is clearly, demonstrably better than OpenClaw? The differentiators above are real but incremental. The "why ALMS?" story needs sharpening — ideally grounded in a concrete scenario where a user would choose ALMS over OpenClaw and be glad they did.

---

*Research by Tesla (2026-03-15). Sources: GitHub API, OpenClaw releases page, web search.*
