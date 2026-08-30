//! Workspace tools — let the agent update and re-read its own workspace
//! files.
//!
//! `workspace_write` is the older of the two. `workspace_read` (#1310) exists
//! because `workspace_write`'s replacing mode is now refused when it would
//! delete text the agent has not been shown, and a refusal is only useful if
//! there is a way to become un-refused: the system-prompt injection is
//! capped and, for `user.md` in a non-user-facing run, absent, so an agent
//! that genuinely means to rewrite a file needs a way to ask for it.

use crate::workspace::{AgentWorkspace, CheckedWrite, RefusedWrite, WorkspaceFile};
use alms_sandbox::{SandboxError, Tool, error::SandboxResult};
use serde_json::Value;

/// Resolve the `file` parameter shared by both workspace tools.
///
/// Returns the caller's spelling alongside the resolved file, because the
/// result echoes the former back and the latter is what does the work.
fn parse_file_param(params: &Value) -> SandboxResult<(&str, WorkspaceFile)> {
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

    Ok((file_str, workspace_file))
}

/// What a refused `mode: "write"` tells the model (#1310).
///
/// Three requirements, and the shape follows from them:
///
/// 1. **Say that nothing happened.** A refusal the model reads as a partial
///    success is worse than no refusal at all.
/// 2. **Say which of the three things went wrong**, because they are not the
///    same situation: a file it has never seen, a file it has seen the end
///    of, and a file that moved under it.
/// 3. **Offer a recovery that cannot itself be unavailable.** `mode:
///    "append"` is the same tool the model is already calling, so it is
///    reachable whenever this message is; `workspace_read` is a separate tool
///    and subject to the same `tools.enabled` allowlist, so it is offered
///    second and only for the case that actually needs it — a genuine
///    whole-file rewrite. `fs_read` is deliberately not mentioned: the
///    workspace does sit inside the sandbox root and an agent with `fs_read`
///    can reach it by path, but nothing guarantees that tool is enabled and
///    the path is not part of any contract the model has been given.
fn refusal_message(file: WorkspaceFile, refusal: RefusedWrite) -> String {
    let name = file.filename();
    let why = match refusal {
        RefusedWrite::NeverShown => format!(
            "{name} is not empty and its contents were not in your context, so replacing it \
             would delete text you have never seen."
        ),
        RefusedWrite::ShownPartially => format!(
            "you have been shown only part of {name} -- the copy in your context is a window \
             onto the end of the file -- so replacing it would delete everything above the cut."
        ),
        RefusedWrite::ChangedSinceShown => format!(
            "{name} has changed since the copy in your context was read, so replacing it would \
             delete whatever changed."
        ),
    };

    format!(
        "Refused, and nothing was written: {why} To add to {name} without deleting anything, \
         call workspace_write again with mode \"append\" and only the new text. To replace the \
         whole file, call workspace_read for this file first -- it returns the current \
         contents -- and build your replacement from what it gives you."
    )
}

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
                                    whenever you want the other one. Note that 'write' discards everything not \
                                    in the content you send — including any entry added since the copy of the file \
                                    in your context was read; send only the new fact and let it append unless you \
                                    are deliberately rewriting the whole file. A 'write' that would delete text \
                                    you have not been shown is refused rather than performed: call workspace_read \
                                    for the file first when you mean to rewrite it."
                }
            },
            "required": ["file", "content"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let (file_str, workspace_file) = parse_file_param(&params)?;

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
        // Only the replacing branch is guarded (#1310). An append cannot
        // delete anything, so there is nothing for a shown-view check to
        // protect and refusing one would remove the recovery this tool
        // offers for the branch that *is* guarded.
        let outcome = tokio::task::spawn_blocking(move || {
            if append {
                workspace
                    .append_file(workspace_file, &content)
                    .map(|()| CheckedWrite::Written)
            } else {
                workspace.write_file_checked(workspace_file, &content)
            }
        })
        .await
        .map_err(|e| SandboxError::Internal(format!("Workspace write task failed: {}", e)))??;

        match outcome {
            CheckedWrite::Written => Ok(serde_json::json!({
                "ok": true,
                "file": file_str,
                "mode": mode,
            })),
            // An in-band failure rather than `Err(SandboxError)`. Both reach
            // the model as tool-result content and both continue the loop
            // (`persist_one_tool_result`), and `tool_result_ok` already reads
            // a top-level `error` key as a failed call (#1048) — so the
            // status is not lost. What the in-band shape adds is structure:
            // `refused` is a stable token the model, the UI and a test can
            // branch on without parsing prose, and the message is not wrapped
            // in a `SandboxError` variant whose Display prefix ("Invalid
            // parameters", "IO error") would misdescribe what happened.
            CheckedWrite::Refused(refusal) => Ok(serde_json::json!({
                "ok": false,
                "file": file_str,
                "mode": mode,
                "refused": refusal.code(),
                "error": refusal_message(workspace_file, refusal),
            })),
        }
    }

    fn is_builtin(&self) -> bool {
        true
    }
}

/// Built-in tool that lets the agent read its own workspace files back.
///
/// The counterpart to `workspace_write`'s refusal (#1310), and the answer to
/// a gap that predates it: nothing on the tool surface read a workspace file
/// by name. The bytes were never unreachable — the workspace lives at
/// `<project_root>/.alms/agents/<name>/`, inside the project-root sandbox, so
/// an agent with `fs_read` enabled could always read the file by path — but
/// nothing hands the model that path, and `fs_read` is not guaranteed
/// enabled. This makes the read addressable the same way the write is: by
/// name, on the same four files, with no path in the contract.
#[derive(Debug, Clone)]
pub struct WorkspaceReadTool {
    workspace: AgentWorkspace,
}

impl WorkspaceReadTool {
    pub fn new(workspace: AgentWorkspace) -> Self {
        Self { workspace }
    }
}

#[async_trait::async_trait]
impl Tool for WorkspaceReadTool {
    fn name(&self) -> &str {
        "workspace_read"
    }

    fn description(&self) -> &str {
        "Read one of the agent's own workspace files (personality.md, goals.md, memories.md, \
         user.md) as it is on disk right now. Call this before replacing a file with \
         workspace_write mode 'write': the copy in your context may be only the end of a long \
         file, may be missing entirely, or may have changed since it was read, and a \
         replacement built from it would delete the rest."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "enum": ["personality", "goals", "memories", "user"],
                    "description": "Which workspace file to read. \
                                    'personality' for the agent's tone/style/role, \
                                    'goals' for current objectives, \
                                    'memories' for learned facts and domain knowledge, \
                                    'user' for who the user is (name, preferences, background)."
                }
            },
            "required": ["file"]
        })
    }

    async fn execute(&self, params: Value) -> SandboxResult<Value> {
        let (file_str, workspace_file) = parse_file_param(&params)?;

        // Blocking file IO on the blocking pool, the same convention the
        // write side follows. This read takes no lock — `read_file` does not,
        // by the reasoning on `AgentWorkspace::lock_path` — so it cannot
        // wedge on another writer, but it can still stall on a slow
        // filesystem.
        let workspace = self.workspace.clone();
        let read = tokio::task::spawn_blocking(move || workspace.read_for_agent(workspace_file))
            .await
            .map_err(|e| SandboxError::Internal(format!("Workspace read task failed: {}", e)))?;

        let mut result = serde_json::json!({
            "ok": true,
            "file": file_str,
            "content": read.content,
            "bytes": read.total_bytes,
            "complete": read.complete,
        });

        if !read.complete {
            // `complete: false` is the machine-readable half; this is the
            // half the model reads. It has to say which end was kept, because
            // a tail window that does not announce itself is exactly the trap
            // #1311 put a marker on the injection to avoid.
            result["note"] = serde_json::json!(format!(
                "Truncated: this is the last {} of {} bytes of {}, not the whole file. \
                 A workspace_write with mode \"write\" built from it will be refused, because \
                 it would delete everything above the cut -- use mode \"append\" instead.",
                read.content.len(),
                read.total_bytes,
                workspace_file.filename()
            ));
        }

        Ok(result)
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

    /// Both workspace tools over **one** `AgentWorkspace`, which is how
    /// `AgentRuntime::with_workspace` builds them: it clones a single
    /// workspace into each, so the record of what the agent has been shown
    /// is shared. Two independently constructed workspaces would not share
    /// it, and `workspace_read` would then unblock nothing.
    fn test_tools() -> (TempDir, WorkspaceWriteTool, WorkspaceReadTool) {
        let dir = TempDir::new().unwrap();
        let workspace = AgentWorkspace::new(dir.path(), "test-agent");
        (
            dir,
            WorkspaceWriteTool::new(workspace.clone()),
            WorkspaceReadTool::new(workspace),
        )
    }

    /// Numbered entries totalling at least `bytes`.
    fn memories_of_at_least(bytes: usize) -> String {
        let mut out = String::new();
        let mut n = 0;
        while out.len() < bytes {
            out.push_str(&format!("- entry {n}\n"));
            n += 1;
        }
        out
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
            // The context build, which is what shows the agent the file.
            // Without it the replacement below is refused as `never_shown`
            // (#1310) and this test would be asserting the guard rather than
            // the default. In a real run this call is not optional: every run
            // assembles the system prompt before the agent gets a turn.
            let _ = tool.workspace.build_system_prompt_prefix(true);

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
    /// smallest thing that reproduces: a run can reach the same end state with
    /// no second agent at all, by appending a few memories and then tidying up
    /// with an explicit `mode: "write"` before it finishes. That single-agent
    /// route is narrower than this comment once claimed — it said
    /// `build_context` runs *once per run*, but `agent_loop` re-reads the
    /// workspace after every tool batch (#1310), so it needs the append and
    /// the replacement to share one batch — and it is now refused outright by
    /// the shown-view guard rather than merely defaulted away from.
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
        // `mode: "write"`. Both halves of that used to lapse past
        // `MEMORIES_INJECTION_CAP`, where the injected window was the file's
        // *head*: the duplicate sat in the truncated tail, unseen, and the
        // repair could only resend the head. #1308 anchors the window to the
        // tail, so a fresh duplicate is inside it either way. See
        // `WorkspaceFile::default_write_mode` for why unread-but-present was
        // still the right side of the trade even before that.
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
        // The context build. Under the cap it shows the file whole, which is
        // what the escape hatch needs: #1310 refuses a replacement the agent
        // could not have composed correctly, not one it could.
        let _ = tool.workspace.build_system_prompt_prefix(false);

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

    // ── #1310: the tool surface of the shown-view guard ────────────────────

    /// The acceptance sequence, through the tool, with no concurrency: an
    /// agent appends three memories during a run and then tidies up with an
    /// explicit `mode: "write"` built from the snapshot it was given.
    ///
    /// Every step is a real call. `build_system_prompt_prefix` is the read
    /// the context build performs; the three appends are `workspace_write`
    /// calls the model made itself; the replacement is the model editing the
    /// only copy of the file it holds. Before #1310 the result was a
    /// well-formed `memories.md` with `- M3`, `- M4` and `- M5` silently
    /// gone.
    ///
    /// No seam is armed and nothing races. The staleness is structural:
    /// `snapshot` is an immutable `String` captured before the appends and
    /// unreachable from them, so no scheduling can make it fresh — the same
    /// property that made #1305's interleaving test deterministic.
    #[tokio::test]
    async fn appending_then_replacing_from_the_run_start_snapshot_is_refused() {
        let (_dir, tool) = test_tool();
        tool.workspace
            .write_file(WorkspaceFile::Memories, "- M1\n- M2")
            .unwrap();

        let snapshot = tool
            .workspace
            .build_system_prompt_prefix(false)
            .strip_prefix("## Memories\n")
            .expect("only memories.md is seeded, so it is the whole prefix")
            .to_string();

        for entry in ["- M3", "- M4", "- M5"] {
            let result = tool
                .execute(serde_json::json!({
                    "file": "memories",
                    "content": entry,
                    "mode": "append",
                }))
                .await
                .unwrap();
            assert_eq!(result["ok"], true, "the appends themselves must succeed");
        }

        let result = tool
            .execute(serde_json::json!({
                "file": "memories",
                "content": snapshot.replace("- M1", "- M1 (corrected)"),
                "mode": "write",
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(result["refused"], "changed_since_shown");

        let on_disk = tool.workspace.read_file(WorkspaceFile::Memories).unwrap();
        for entry in ["- M3", "- M4", "- M5"] {
            assert!(
                on_disk.contains(entry),
                "{entry} was written during this run and must survive the tidy-up; \
                 memories.md is now:\n{on_disk}"
            );
        }
        assert!(
            !on_disk.contains("(corrected)"),
            "a refused write must land nothing at all, not partially; \
             memories.md is now:\n{on_disk}"
        );
    }

    /// The case that needs no appends, no second agent and no batching: past
    /// the injection cap the agent has only ever seen the end of
    /// `memories.md`, so the replacement it sends back deletes the rest.
    ///
    /// The agent modelled here does everything right — it returns exactly the
    /// window it was shown, and nothing has touched the file in between.
    #[tokio::test]
    async fn replacing_an_over_cap_memories_file_from_its_window_is_refused() {
        let (_dir, tool) = test_tool();
        let memories = memories_of_at_least(crate::workspace::MEMORIES_INJECTION_CAP + 500);
        tool.workspace
            .write_file(WorkspaceFile::Memories, &memories)
            .unwrap();

        let prefix = tool.workspace.build_system_prompt_prefix(false);
        let window = prefix
            .strip_prefix("## Memories\n")
            .expect("only memories.md is seeded");
        assert!(
            window.contains("Older memories truncated"),
            "precondition: the agent is shown a window, not the file"
        );

        let result = tool
            .execute(serde_json::json!({
                "file": "memories",
                "content": window,
                "mode": "write",
            }))
            .await
            .unwrap();

        assert_eq!(result["ok"], false);
        assert_eq!(result["refused"], "shown_partially");
        assert_eq!(
            tool.workspace.read_file(WorkspaceFile::Memories).unwrap(),
            memories,
            "the entries above the cut must survive"
        );
    }

    /// `user.md` in a non-user-facing run, which is the worst of the four
    /// cases and the least visible: the prompt omits the file entirely, and
    /// `user` defaults to `mode: "write"`, so the model does not even have to
    /// ask for a replacement to destroy one.
    ///
    /// The user-facing row is not decoration. Without it, a fix that simply
    /// never let anyone write `user.md` would pass.
    #[tokio::test]
    async fn a_non_user_facing_run_cannot_blind_write_user_md() {
        for (include_user, expected_ok) in [(true, true), (false, false)] {
            let (_dir, tool) = test_tool();
            tool.workspace
                .write_file(WorkspaceFile::User, "Name: Alper\nPrefers concise answers.")
                .unwrap();
            let _ = tool.workspace.build_system_prompt_prefix(include_user);

            // No `mode` — `user` defaults to "write", so this is the shape an
            // ordinary "I learned something about the user" call takes.
            let result = tool
                .execute(serde_json::json!({
                    "file": "user",
                    "content": "Name: Alper",
                }))
                .await
                .unwrap();

            assert_eq!(
                result["mode"], "write",
                "precondition: user defaults to write"
            );
            assert_eq!(result["ok"], expected_ok, "include_user={include_user}");

            let on_disk = tool.workspace.read_file(WorkspaceFile::User).unwrap();
            assert_eq!(
                on_disk.contains("Prefers concise answers."),
                !include_user,
                "include_user={include_user}: the preference line survives exactly when \
                 the run never saw it; user.md is now:\n{on_disk}"
            );
        }
    }

    /// What the refusal actually tells the model, asserted because it is the
    /// entire recovery path: a refusal the model cannot act on is a worse
    /// outcome than the silent loss it replaces.
    ///
    /// Three claims, one per requirement on `refusal_message`: that nothing
    /// was written, that `mode: "append"` is available, and that
    /// `workspace_read` is the way to do the rewrite. Plus the one that makes
    /// the shape work at all — `tool_result_ok` must read it as a failed
    /// call, or the UI and the audit log record a success.
    #[tokio::test]
    async fn the_refusal_says_nothing_was_written_and_names_both_recoveries() {
        let (_dir, tool) = test_tool();
        tool.workspace
            .write_file(WorkspaceFile::Goals, "- ship the thing")
            .unwrap();

        let result = tool
            .execute(serde_json::json!({
                "file": "goals",
                "content": "- something else",
                "mode": "write",
            }))
            .await
            .unwrap();

        assert_eq!(result["refused"], "never_shown");
        let message = result["error"]
            .as_str()
            .expect("a refusal carries a message");
        assert!(
            message.contains("nothing was written"),
            "the model must not read a refusal as a partial success: {message}"
        );
        assert!(
            message.contains("goals.md"),
            "the message must name the file it is about: {message}"
        );
        assert!(
            message.contains("\"append\""),
            "the recovery that is always available is the append mode of this same tool: \
             {message}"
        );
        assert!(
            message.contains("workspace_read"),
            "and the recovery for a genuine rewrite is the read tool: {message}"
        );
        assert!(
            !message.contains("fs_read"),
            "fs_read is not guaranteed enabled and the workspace path is not part of any \
             contract the model has: {message}"
        );

        assert!(
            !crate::agent::helpers::tool_result_ok(&result),
            "a refusal must count as a failed tool call, or the run records a success: {result}"
        );
    }

    /// The recovery, through both tools, in the order an agent would use
    /// them: refused, read, replaced.
    ///
    /// This is also what pins that the two tools share one record of what the
    /// agent has been shown. Built over separate `AgentWorkspace` values they
    /// would each keep their own, the read would record into a record the
    /// write never consults, and the second replacement would be refused
    /// again — a livelock rather than a data loss, but not a fix.
    ///
    /// **Sharing the record turned out to be necessary and not sufficient**,
    /// and the first version of this test said otherwise while shipping the
    /// livelock it ruled out. The record was shared; the prompt rebuild
    /// between the two batches overwrote it anyway. The rebuild below is the
    /// difference, and it is the production step, not a scenario: read and
    /// write cannot be one batch.
    #[tokio::test]
    async fn a_workspace_read_unblocks_the_refused_replacement() {
        let (_dir, write, read) = test_tools();
        let memories = memories_of_at_least(crate::workspace::MEMORIES_INJECTION_CAP + 500);
        write
            .workspace
            .write_file(WorkspaceFile::Memories, &memories)
            .unwrap();
        let _ = write.workspace.build_system_prompt_prefix(false);

        let refused = write
            .execute(serde_json::json!({
                "file": "memories",
                "content": "- entry 0 and everything after it, compacted",
                "mode": "write",
            }))
            .await
            .unwrap();
        assert_eq!(refused["refused"], "shown_partially");

        let read_result = read
            .execute(serde_json::json!({"file": "memories"}))
            .await
            .unwrap();
        assert_eq!(read_result["ok"], true);
        assert_eq!(read_result["complete"], true);
        assert_eq!(read_result["bytes"], memories.len());
        assert_eq!(
            read_result["content"].as_str().unwrap(),
            memories,
            "an uncapped read is the file verbatim -- the point of it is that the agent \
             can rewrite from the whole thing"
        );
        assert!(
            read_result.get("note").is_none(),
            "a complete read has nothing to warn about: {read_result}"
        );

        // The prompt rebuild that `agent_loop` performs after every tool
        // batch. It belongs between these two calls and cannot be anywhere
        // else: the read and the write are necessarily separate batches,
        // because the model has to see the read result before it can compose
        // the replacement.
        let _ = write.workspace.build_system_prompt_prefix(false);

        let written = write
            .execute(serde_json::json!({
                "file": "memories",
                "content": "- entry 0 and everything after it, compacted",
                "mode": "write",
            }))
            .await
            .unwrap();
        assert_eq!(written["ok"], true, "{written}");
        assert_eq!(
            write.workspace.read_file(WorkspaceFile::Memories).unwrap(),
            "- entry 0 and everything after it, compacted"
        );
    }

    /// A read that hits its own cap says so and does **not** unblock the
    /// replacement.
    ///
    /// The tempting shape is "a read always makes the next write legal";
    /// that would turn the recovery into a laundering step, handing a
    /// `WORKSPACE_READ_CAP`-sized window permission to delete a much larger
    /// file. The `note` is asserted
    /// alongside the flag because the flag is for the machine and the note is
    /// the only part the model reads.
    #[tokio::test]
    async fn a_capped_read_says_so_and_leaves_the_replacement_refused() {
        let (_dir, write, read) = test_tools();
        let memories = memories_of_at_least(crate::workspace::WORKSPACE_READ_CAP + 500);
        write
            .workspace
            .write_file(WorkspaceFile::Memories, &memories)
            .unwrap();

        let read_result = read
            .execute(serde_json::json!({"file": "memories"}))
            .await
            .unwrap();
        assert_eq!(read_result["complete"], false);
        assert_eq!(read_result["bytes"], memories.len());
        let note = read_result["note"]
            .as_str()
            .expect("a capped read carries a note");
        assert!(note.contains("not the whole file"), "{note}");
        assert!(
            note.contains("last"),
            "the note must say which end was kept: {note}"
        );
        assert!(
            read_result["content"].as_str().unwrap().len() <= crate::workspace::WORKSPACE_READ_CAP
        );

        let refused = write
            .execute(serde_json::json!({
                "file": "memories",
                "content": "- compacted",
                "mode": "write",
            }))
            .await
            .unwrap();
        assert_eq!(
            refused["refused"], "shown_partially",
            "a capped read must not launder a partial view into permission to replace"
        );
    }

    /// A read of a file that is not there is an empty, complete read — and
    /// the write that follows it goes through, because there was never
    /// anything to lose.
    #[tokio::test]
    async fn reading_a_missing_file_is_empty_and_complete() {
        let (_dir, write, read) = test_tools();

        let read_result = read
            .execute(serde_json::json!({"file": "personality"}))
            .await
            .unwrap();
        assert_eq!(read_result["ok"], true);
        assert_eq!(read_result["content"], "");
        assert_eq!(read_result["bytes"], 0);
        assert_eq!(read_result["complete"], true);

        let written = write
            .execute(serde_json::json!({
                "file": "personality",
                "content": "I am a helpful assistant.",
                "mode": "write",
            }))
            .await
            .unwrap();
        assert_eq!(written["ok"], true);
    }

    /// The read tool answers for all four files, by the same bare names the
    /// write tool takes.
    ///
    /// Enumerated rather than spot-checked: the two tools share one parameter
    /// parser, and a claim about "the workspace files" should cover the set
    /// the schema advertises.
    #[tokio::test]
    async fn the_read_tool_addresses_all_four_files_by_name() {
        let (_dir, write, read) = test_tools();
        for (file_str, file) in [
            ("personality", WorkspaceFile::Personality),
            ("goals", WorkspaceFile::Goals),
            ("memories", WorkspaceFile::Memories),
            ("user", WorkspaceFile::User),
        ] {
            write
                .workspace
                .write_file(file, &format!("contents of {file_str}"))
                .unwrap();

            let result = read
                .execute(serde_json::json!({"file": file_str}))
                .await
                .unwrap();
            assert_eq!(result["file"], file_str);
            assert_eq!(result["content"], format!("contents of {file_str}"));
        }

        let schema = read.parameters();
        let names: Vec<&str> = schema["properties"]["file"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(names, ["personality", "goals", "memories", "user"]);
        assert_eq!(
            schema["required"].as_array().unwrap(),
            &[serde_json::json!("file")]
        );
    }

    /// The read tool rejects the same bad parameters the write tool does, as
    /// hard errors rather than in-band ones — an unparseable request is a
    /// different thing from a refusal, and only the latter is an answer about
    /// the file.
    #[tokio::test]
    async fn the_read_tool_rejects_a_missing_or_unknown_file() {
        let (_dir, _write, read) = test_tools();

        let err = read.execute(serde_json::json!({})).await.unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)), "{err}");

        let err = read
            .execute(serde_json::json!({"file": "secrets"}))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidParameters(_)), "{err}");
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
