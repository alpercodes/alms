//! workspace_write tool — lets the agent update its own workspace files.

use crate::workspace::{AgentWorkspace, WorkspaceFile};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use serde_json::Value;

/// Built-in tool that lets the agent write to its own workspace files.
#[derive(Debug, Clone)]
pub struct WorkspaceWriteTool {
    workspace: AgentWorkspace,
}

impl WorkspaceWriteTool {
    pub fn new(workspace: AgentWorkspace) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for WorkspaceWriteTool {
    fn name(&self) -> &str {
        "workspace_write"
    }

    fn description(&self) -> &str {
        "Write or append to the agent's own workspace files (personality.md, goals.md, memories.md, user.md). \
         Use this to persist identity, goals, memories, and user info across conversations."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "enum": ["personality", "goals", "memories", "user"],
                    "description": "Which workspace file to write. \
                                    'personality' for the agent's tone/style/role, \
                                    'goals' for current objectives, \
                                    'memories' for learned facts and domain knowledge, \
                                    'user' for who the user is (name, preferences, background)."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write."
                },
                "mode": {
                    "type": "string",
                    "enum": ["write", "append"],
                    "description": "Whether to replace the whole file ('write') or add to the end of it ('append'). \
                                    Defaults to 'append' for 'memories', which accumulates, and to 'write' for \
                                    'personality', 'goals' and 'user', which are restated. Say which you mean \
                                    whenever you want the other one. Note that 'write' on 'memories' discards \
                                    everything not in the content you send — including any entry added since the \
                                    copy of the file in your context was read; send only the new fact and let it \
                                    append unless you are deliberately rewriting the whole file."
                }
            },
            "required": ["file", "content"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let file_str = params
            .get("file")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'file' is required".to_string()))?;

        let workspace_file = match file_str {
            "personality" => WorkspaceFile::Personality,
            "goals" => WorkspaceFile::Goals,
            "memories" => WorkspaceFile::Memories,
            "user" => WorkspaceFile::User,
            other => {
                return Err(SandboxError::InvalidParameters(format!(
                    "Unknown file '{}': must be 'personality', 'goals', 'memories', or 'user'",
                    other
                )));
            }
        };

        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| SandboxError::InvalidParameters("'content' is required".to_string()))?;

        // Per-file default rather than a flat `"write"`, because the two
        // branches are not equally safe to guess: see
        // `WorkspaceFile::default_write_mode` for the decision and its cost
        // (#1305). The result below echoes `mode`, so what an omitted
        // parameter resolved to is visible to the agent in the same turn.
        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| workspace_file.default_write_mode());

        let append = match mode {
            "write" => false,
            "append" => true,
            other => {
                return Err(SandboxError::InvalidParameters(format!(
                    "Unknown mode '{}': must be 'write' or 'append'",
                    other
                )));
            }
        };

        // Both writers do blocking file IO, and both wait on the file's
        // cross-process advisory lock with no timeout (#1280 for the append,
        // #1294 for the write). Awaiting that on a tokio worker would wedge
        // the worker for as long as some other holder — a second daemon on
        // the same data dir — keeps the lock. Offloaded to the blocking pool
        // instead, the same convention `check_sandbox_path_async` follows for
        // far cheaper work.
        let workspace = self.workspace.clone();
        let content = content.to_string();
        tokio::task::spawn_blocking(move || {
            if append {
                workspace.append_file(workspace_file, &content)
            } else {
                workspace.write_file(workspace_file, &content)
            }
        })
        .await
        .map_err(|e| SandboxError::Internal(format!("Workspace write task failed: {}", e)))??;

        Ok(serde_json::json!({
            "ok": true,
            "file": file_str,
            "mode": mode,
        }))
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_tool() -> (TempDir, WorkspaceWriteTool) {
        let dir = TempDir::new().unwrap();
        let workspace = AgentWorkspace::new(dir.path(), "test-agent");
        let tool = WorkspaceWriteTool::new(workspace);
        (dir, tool)
    }

    #[tokio::test]
    async fn test_write_goals() {
        let (_dir, tool) = test_tool();
        let result = tool
            .execute(serde_json::json!({
                "file": "goals",
                "content": "Build the MVP"
            }))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);

        let content = tool.workspace.read_file(WorkspaceFile::Goals).unwrap();
        assert_eq!(content, "Build the MVP");
    }

    #[tokio::test]
    async fn test_append_memories() {
        let (_dir, tool) = test_tool();
        tool.execute(serde_json::json!({
            "file": "memories",
            "content": "Fact 1",
            "mode": "write"
        }))
        .await
        .unwrap();
        tool.execute(serde_json::json!({
            "file": "memories",
            "content": "Fact 2",
            "mode": "append"
        }))
        .await
        .unwrap();

        let content = tool.workspace.read_file(WorkspaceFile::Memories).unwrap();
        assert!(content.contains("Fact 1"));
        assert!(content.contains("Fact 2"));
    }

    #[tokio::test]
    async fn test_personality_writable() {
        let (_dir, tool) = test_tool();
        tool.execute(serde_json::json!({
            "file": "personality",
            "content": "I am a helpful assistant."
        }))
        .await
        .unwrap();
        let content = tool
            .workspace
            .read_file(WorkspaceFile::Personality)
            .unwrap();
        assert!(content.contains("helpful assistant"));
    }

    #[tokio::test]
    async fn test_unknown_file_rejected() {
        let (_dir, tool) = test_tool();
        let err = tool
            .execute(serde_json::json!({
                "file": "secrets",
                "content": "oops"
            }))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_missing_content_rejected() {
        let (_dir, tool) = test_tool();
        let err = tool
            .execute(serde_json::json!({"file": "goals"}))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)));
    }

    #[tokio::test]
    async fn test_write_user() {
        let (_dir, tool) = test_tool();
        let result = tool
            .execute(serde_json::json!({
                "file": "user",
                "content": "Name: Alper. Prefers concise answers."
            }))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
        assert_eq!(result["file"], "user");

        let content = tool.workspace.read_file(WorkspaceFile::User).unwrap();
        assert!(content.contains("Alper"));
    }

    /// `mode` is optional, so this is the branch an LLM takes whenever it
    /// does not think to ask for one. For `memories` that branch is now the
    /// locked *append* (#1305), which is pinned two ways: the older fact is
    /// still there afterwards, and the result reports the mode the omitted
    /// parameter actually resolved to rather than the literal it used to
    /// default to.
    ///
    /// The sidecar lock assertion is #1294's, carried over verbatim: an
    /// omitted `mode` must still be serialised against every other writer,
    /// and nothing else in a workspace creates that file.
    ///
    /// The *assertion* is unchanged; the **guarantee behind it is weaker**,
    /// and deliberately so. The old default routed to `replace_file`, where
    /// an unavailable lock *refuses* the write ("Refusing to replace … without
    /// its lock"); the memories default now routes to `append_file`, whose
    /// lock failure is warn-and-step-over. So an omitted `mode` on memories
    /// moved from serialised-**or-refused** to serialised-**best-effort**, and
    /// this assertion is satisfied by both and cannot tell them apart. That is
    /// the right weakening rather than a regression: #1292's proof travels
    /// with the policy here, because the destination really is a
    /// non-destructive append-mode write, so an unserialised one costs at most
    /// a misplaced separator — the exact reasoning #1294 said it could *not*
    /// borrow for a replacement.
    #[tokio::test]
    async fn the_memories_default_now_appends_under_the_same_lock() {
        let (_dir, tool) = test_tool();
        tool.workspace
            .write_file(WorkspaceFile::Memories, "- an older fact")
            .unwrap();

        let result = tool
            .execute(serde_json::json!({
                "file": "memories",
                "content": "- learned a thing"
            }))
            .await
            .unwrap();
        assert_eq!(
            result["mode"], "append",
            "an omitted `mode` on memories must resolve to 'append' (#1305)"
        );

        let dir = tool.workspace.dir();
        assert_eq!(
            std::fs::read_to_string(dir.join("memories.md")).unwrap(),
            "- an older fact\n- learned a thing",
            "the default must add to memories.md, not replace it"
        );
        assert!(
            dir.join(".memories.md.lock").exists(),
            "a default-mode workspace_write must take the file's sidecar lock; \
             files on disk: {:?}",
            std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect::<Vec<_>>()
        );
    }

    /// The other half of the per-file default: the three identity files each
    /// describe one settled thing and are meant to be restated, so an omitted
    /// `mode` on them still means whole-file replacement — through the locked,
    /// staged replacement of #1294.
    ///
    /// The old content being *gone* is the discriminator. Once `memories`
    /// appends by default, the sidecar lock no longer distinguishes the two
    /// branches — both writers take it — so only the file's contents say
    /// which one ran.
    ///
    /// Each file then takes a second call with an explicit `mode: "append"`,
    /// which must be honoured: the per-file rule is a **default**, not a
    /// clamp on which files may append. That is the mirror of
    /// [`an_explicit_write_mode_still_replaces_memories`] and the only thing
    /// standing between the two of them and a policy that silently ignores
    /// half of what the schema advertises.
    #[tokio::test]
    async fn the_identity_files_default_to_replacement_and_still_honour_an_explicit_append() {
        for (file_str, file) in [
            ("personality", WorkspaceFile::Personality),
            ("goals", WorkspaceFile::Goals),
            ("user", WorkspaceFile::User),
        ] {
            let (_dir, tool) = test_tool();
            tool.workspace
                .write_file(file, "the stale version")
                .unwrap();

            let result = tool
                .execute(serde_json::json!({
                    "file": file_str,
                    "content": "the fresh version",
                }))
                .await
                .unwrap();
            assert_eq!(
                result["mode"], "write",
                "an omitted `mode` on {file_str} must still resolve to 'write'"
            );

            assert_eq!(
                tool.workspace.read_file(file).unwrap(),
                "the fresh version",
                "{file_str} must be replaced by a default-mode write, not appended to"
            );
            let lock = tool
                .workspace
                .dir()
                .join(format!(".{}.lock", file.filename()));
            assert!(
                lock.exists(),
                "the replacement of {file_str} must take the file's sidecar lock (#1294)"
            );

            // The mirror of `an_explicit_write_mode_still_replaces_memories`,
            // and the reason it is needed: the per-file rule is a **default**,
            // not a clamp. Without this, `append = mode == "append" &&
            // is_memories` — per-file policy applied as a restriction on which
            // files may append at all — passes the whole suite, because every
            // other identity-file case omits `mode` and every explicit-append
            // case targets memories. This PR is what creates that asymmetry,
            // so it is what has to close it.
            let result = tool
                .execute(serde_json::json!({
                    "file": file_str,
                    "content": "an explicitly appended line",
                    "mode": "append",
                }))
                .await
                .unwrap();
            assert_eq!(result["mode"], "append");
            let content = tool.workspace.read_file(file).unwrap();
            assert!(
                content.contains("the fresh version"),
                "an explicit `mode: \"append\"` on {file_str} must be honoured, \
                 not clamped to its 'write' default; {} is now:\n{content}",
                file.filename()
            );
            assert!(
                content.contains("an explicitly appended line"),
                "and the appended line must land; {} is now:\n{content}",
                file.filename()
            );
        }
    }

    /// The #1305 interleaving, pinned end to end: a memory appended after the
    /// run's context was built is still on disk after the model's default-mode
    /// `workspace_write` lands.
    ///
    /// Every step is a real call on the real path — `build_system_prompt_prefix`
    /// is the read the context build performs, `append_file` is what another
    /// live instance of the same named agent does, and `execute` with no
    /// `mode` is what an LLM that did not think to ask for one sends.
    ///
    /// **No seam is armed, and the test is still airtight.** The #1292/#1302
    /// seams exist to place a competitor *inside* a critical section, and the
    /// competitor they model has to bypass the lock. Here there is no critical
    /// section and nothing to bypass, so a seam would have nothing to model.
    /// What makes this deterministic instead is that the staleness is
    /// **structural**: `snapshot` is an immutable `String`, captured before
    /// the append and unreachable from it, so no scheduling can make it fresh.
    /// Same virtue as holding a `File` handle across a write in #1302 — turn
    /// the race into a structural observation and the timing disappears.
    ///
    /// The sequence also understates the real window on purpose, by being the
    /// smallest thing that reproduces: `build_context` runs *once per run*, so
    /// a run can reach the same end state with no second agent at all, by
    /// appending a few memories and then tidying up with an explicit
    /// `mode: "write"` before it finishes.
    ///
    /// Before #1305 the file ended up equal to the edited snapshot and `- M6`
    /// was gone — no error, no warning, a well-formed file.
    #[tokio::test]
    async fn a_concurrent_append_survives_a_default_mode_memories_write() {
        let (_dir, tool) = test_tool();
        tool.workspace
            .write_file(WorkspaceFile::Memories, "- M1\n- M2\n- M3\n- M4\n- M5")
            .unwrap();

        // 1. The context build injects memories.md into the system prompt.
        //    This string is what the model looks at for the rest of the run.
        //
        //    The prefix is exactly the memories section because `test_tool()`
        //    writes no personality/goals/user — `build_system_prompt_prefix`
        //    joins only the files that exist, and it is called with
        //    `include_user: false` besides. Seeding any of them above would
        //    break the `strip_prefix` rather than the property under test, so
        //    the `expect` names that precondition rather than the assertion.
        let snapshot = tool
            .workspace
            .build_system_prompt_prefix(false)
            .strip_prefix("## Memories\n")
            .expect("only memories.md is seeded, so it is the whole prefix")
            .to_string();
        assert!(snapshot.contains("- M5"));
        assert!(!snapshot.contains("- M6"), "M6 does not exist yet");

        // 2. A concurrent writer appends M6 — correctly, atomically, under
        //    the lock (#1280). The model cannot see it: its context was built
        //    in step 1.
        tool.workspace
            .append_file(WorkspaceFile::Memories, "- M6")
            .unwrap();

        // 3. The model edits the snapshot it was shown and sends it back
        //    without a `mode`.
        let result = tool
            .execute(serde_json::json!({
                "file": "memories",
                "content": snapshot.replace("- M3", "- M3 (corrected)"),
            }))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);

        let content = tool.workspace.read_file(WorkspaceFile::Memories).unwrap();
        assert!(
            content.contains("- M6"),
            "the concurrent append must survive a default-mode workspace_write; \
             memories.md is now:\n{content}"
        );
        assert!(
            content.contains("- M3 (corrected)"),
            "and the model's own edit must land; memories.md is now:\n{content}"
        );

        // The accepted cost, pinned so it is a decision and not a surprise:
        // a model that omitted `mode` while meaning a wholesale rewrite gets
        // duplicated entries instead of a silent loss. The duplication is
        // visible in the next context build and fixable with an explicit
        // `mode: "write"` *while the file stays under the 4000-char injection
        // cap* — past it the tail is truncated away and neither holds. See
        // `WorkspaceFile::default_write_mode` for why that does not change
        // the decision: unread-but-present is still recoverable, a lost
        // update is not. This file is far under the cap, which is why the
        // duplication is observable here at all.
        assert_eq!(
            content.matches("- M1").count(),
            2,
            "resending the whole file under the append default duplicates it; \
             memories.md is now:\n{content}"
        );
    }

    /// The escape hatch stays open. #1305 changed which branch an *omitted*
    /// `mode` takes, not what `mode: "write"` does — an agent deliberately
    /// pruning or reorganising its memories must still be able to replace the
    /// file, and carries the staleness risk knowingly when it does.
    #[tokio::test]
    async fn an_explicit_write_mode_still_replaces_memories() {
        let (_dir, tool) = test_tool();
        tool.workspace
            .write_file(WorkspaceFile::Memories, "- M1\n- M2")
            .unwrap();

        let result = tool
            .execute(serde_json::json!({
                "file": "memories",
                "content": "- M1 and M2, merged",
                "mode": "write",
            }))
            .await
            .unwrap();
        assert_eq!(result["mode"], "write");

        assert_eq!(
            tool.workspace.read_file(WorkspaceFile::Memories).unwrap(),
            "- M1 and M2, merged",
            "an explicit `mode: \"write\"` must still replace memories.md wholesale"
        );
    }

    #[test]
    fn test_parameters_schema_has_required() {
        let (_dir, tool) = test_tool();
        let schema = tool.parameters();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "file"));
        assert!(required.iter().any(|v| v == "content"));
    }
}
