//! Agent workspace — persistent identity files.
//!
//! Each agent has a workspace directory containing:
//! - personality.md — tone, style, constraints (describes the *agent*)
//! - goals.md — current objectives (agent + user editable)
//! - memories.md — learned facts, domain knowledge (agent + user editable)
//! - user.md — who the user is: name, preferences, background (agent + user editable)
//!
//! These are read at the start of each run and injected into the system prompt.
//! The agent can update goals.md, memories.md, and user.md via the workspace_write tool.

use alms_core::{AlmsError, AlmsResult, truncate_to_char_boundary};
use std::path::PathBuf;
use tracing::{debug, info};

/// Agent workspace — reads and manages persistent agent identity files.
#[derive(Debug, Clone)]
pub struct AgentWorkspace {
    /// Resolved workspace directory for this agent.
    dir: PathBuf,
}

/// Files that can be read/written in the workspace
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceFile {
    Personality,
    Goals,
    Memories,
    User,
}

impl WorkspaceFile {
    pub fn filename(&self) -> &str {
        match self {
            WorkspaceFile::Personality => "personality.md",
            WorkspaceFile::Goals => "goals.md",
            WorkspaceFile::Memories => "memories.md",
            WorkspaceFile::User => "user.md",
        }
    }

    /// Whether the agent is allowed to write this file
    pub fn agent_writable(&self) -> bool {
        match self {
            WorkspaceFile::Personality => true,
            WorkspaceFile::Goals => true,
            WorkspaceFile::Memories => true,
            WorkspaceFile::User => true,
        }
    }

    pub fn all() -> &'static [WorkspaceFile] {
        &[
            WorkspaceFile::Personality,
            WorkspaceFile::Goals,
            WorkspaceFile::Memories,
            WorkspaceFile::User,
        ]
    }
}

impl AgentWorkspace {
    /// Create a workspace at `{base_dir}/{agent_name}/`.
    ///
    /// Standard constructor for top-level agents. Agent names are unique
    /// slug-safe identifiers, giving human-readable workspace paths.
    pub fn new(base_dir: impl Into<PathBuf>, agent_name: &str) -> Self {
        Self {
            dir: base_dir.into().join(agent_name),
        }
    }

    /// Create a workspace that uses `dir` directly as the workspace path.
    ///
    /// Used for subagents whose workspace path is already fully resolved.
    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Get the workspace directory for this agent.
    pub fn dir(&self) -> PathBuf {
        self.dir.clone()
    }

    /// Ensure the workspace directory exists
    pub fn ensure_dir(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(self.dir())
    }

    /// Read a workspace file. Returns None if file doesn't exist or is empty.
    pub fn read_file(&self, file: WorkspaceFile) -> Option<String> {
        let path = self.dir().join(file.filename());
        match std::fs::read_to_string(&path) {
            Ok(content) if !content.trim().is_empty() => {
                debug!("Read workspace file: {}", path.display());
                Some(content)
            }
            Ok(_) => None,  // empty file
            Err(_) => None, // doesn't exist
        }
    }

    /// Write a workspace file. Checks `agent_writable()` before writing.
    pub fn write_file(&self, file: WorkspaceFile, content: &str) -> AlmsResult<()> {
        if !file.agent_writable() {
            return Err(AlmsError::InvalidConfig(format!(
                "{} is not agent-writable (edit it manually)",
                file.filename()
            )));
        }

        self.ensure_dir()
            .map_err(|e| AlmsError::Runtime(format!("Cannot create workspace dir: {}", e)))?;

        let path = self.dir().join(file.filename());
        std::fs::write(&path, content).map_err(|e| {
            AlmsError::Runtime(format!("Failed to write {}: {}", path.display(), e))
        })?;

        info!("Updated workspace file: {}", path.display());
        Ok(())
    }

    /// Append to a workspace file (for memories).
    pub fn append_file(&self, file: WorkspaceFile, content: &str) -> AlmsResult<()> {
        if !file.agent_writable() {
            return Err(AlmsError::InvalidConfig(format!(
                "{} is not agent-writable",
                file.filename()
            )));
        }

        self.ensure_dir()
            .map_err(|e| AlmsError::Runtime(format!("Cannot create workspace dir: {}", e)))?;

        let path = self.dir().join(file.filename());
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let new_content = if existing.is_empty() {
            content.to_string()
        } else {
            format!("{}\n{}", existing.trim_end(), content)
        };

        std::fs::write(&path, new_content).map_err(|e| {
            AlmsError::Runtime(format!("Failed to append to {}: {}", path.display(), e))
        })?;

        info!("Appended to workspace file: {}", path.display());
        Ok(())
    }

    /// Check if this is a fresh agent (no workspace files exist)
    pub fn needs_bootstrap(&self) -> bool {
        // Bootstrap if personality.md doesn't exist
        self.read_file(WorkspaceFile::Personality).is_none()
    }

    /// Build system prompt prefix from workspace files.
    ///
    /// When `include_user` is false, `user.md` is omitted from the prefix.
    /// This saves tokens and avoids confusion in non-user-facing contexts
    /// (DM sessions, subagent runs, scheduled jobs).
    pub fn build_system_prompt_prefix(&self, include_user: bool) -> String {
        let mut parts = Vec::new();

        if let Some(personality) = self.read_file(WorkspaceFile::Personality) {
            parts.push(personality);
        }

        if let Some(goals) = self.read_file(WorkspaceFile::Goals) {
            parts.push(format!("## Current Goals\n{}", goals));
        }

        if include_user && let Some(user) = self.read_file(WorkspaceFile::User) {
            parts.push(format!("## About the User\n{}", user));
        }

        if let Some(memories) = self.read_file(WorkspaceFile::Memories) {
            // Truncate memories if too long (will be properly budgeted by ContextBuilder)
            let memories = if memories.len() > 4000 {
                format!(
                    "{}...\n[memories truncated, {} chars total]",
                    truncate_to_char_boundary(&memories, 4000),
                    memories.len()
                )
            } else {
                memories
            };
            parts.push(format!("## Memories\n{}", memories));
        }

        if parts.is_empty() {
            String::new()
        } else {
            parts.join("\n\n")
        }
    }

    /// Get the bootstrap system prompt for first-time agent setup
    pub fn bootstrap_prompt() -> &'static str {
        include_str!("../prompts/bootstrap.md").trim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_workspace() -> (TempDir, AgentWorkspace) {
        let dir = TempDir::new().unwrap();
        let ws = AgentWorkspace::new(dir.path(), "test-agent");
        (dir, ws)
    }

    #[test]
    fn test_needs_bootstrap_fresh() {
        let (_dir, ws) = test_workspace();
        assert!(ws.needs_bootstrap());
    }

    #[test]
    fn test_needs_bootstrap_with_personality() {
        let (_dir, ws) = test_workspace();
        ws.ensure_dir().unwrap();
        std::fs::write(
            ws.dir().join("personality.md"),
            "I am a helpful coding assistant.",
        )
        .unwrap();
        assert!(!ws.needs_bootstrap());
    }

    #[test]
    fn test_write_and_read() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Goals, "Build the thing")
            .unwrap();
        assert_eq!(
            ws.read_file(WorkspaceFile::Goals).unwrap(),
            "Build the thing"
        );
    }

    #[test]
    fn test_personality_writable() {
        // personality.md is agent-writable so the bootstrap interview can save it.
        let (_dir, ws) = test_workspace();
        let result = ws.write_file(
            WorkspaceFile::Personality,
            "I am a concise coding assistant.",
        );
        assert!(result.is_ok());
        assert_eq!(
            ws.read_file(WorkspaceFile::Personality).unwrap(),
            "I am a concise coding assistant."
        );
    }

    #[test]
    fn test_append() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::Memories, "Fact 1").unwrap();
        ws.append_file(WorkspaceFile::Memories, "Fact 2").unwrap();
        let content = ws.read_file(WorkspaceFile::Memories).unwrap();
        assert!(content.contains("Fact 1"));
        assert!(content.contains("Fact 2"));
    }

    #[test]
    fn test_build_system_prompt_prefix_empty() {
        let (_dir, ws) = test_workspace();
        assert!(ws.build_system_prompt_prefix(true).is_empty());
    }

    #[test]
    fn test_build_system_prompt_prefix_with_files() {
        let (_dir, ws) = test_workspace();
        ws.ensure_dir().unwrap();
        std::fs::write(
            ws.dir().join("personality.md"),
            "I am concise and technical.",
        )
        .unwrap();
        ws.write_file(WorkspaceFile::Goals, "Help with Rust")
            .unwrap();
        ws.write_file(WorkspaceFile::User, "Name: Alper. Prefers concise answers.")
            .unwrap();

        let prefix = ws.build_system_prompt_prefix(true);
        assert!(prefix.contains("concise and technical"));
        assert!(prefix.contains("Help with Rust"));
        assert!(prefix.contains("About the User"));
        assert!(prefix.contains("Alper"));
    }

    #[test]
    fn test_build_system_prompt_prefix_skip_user() {
        let (_dir, ws) = test_workspace();
        ws.ensure_dir().unwrap();
        std::fs::write(
            ws.dir().join("personality.md"),
            "I am concise and technical.",
        )
        .unwrap();
        ws.write_file(WorkspaceFile::Goals, "Help with Rust")
            .unwrap();
        ws.write_file(WorkspaceFile::User, "Name: Alper. Prefers concise answers.")
            .unwrap();

        let prefix = ws.build_system_prompt_prefix(false);
        assert!(prefix.contains("concise and technical"));
        assert!(prefix.contains("Help with Rust"));
        // user.md should be omitted for non-user-facing sessions
        assert!(!prefix.contains("About the User"));
        assert!(!prefix.contains("Alper"));
    }

    #[test]
    fn test_with_dir_uses_path_directly() {
        let dir = TempDir::new().unwrap();
        let ws_dir = dir.path().join("reviewer");
        let ws = AgentWorkspace::with_dir(&ws_dir);
        // dir() should return the exact path, no UUID appended
        assert_eq!(ws.dir(), ws_dir);
        ws.write_file(WorkspaceFile::Goals, "Review code").unwrap();
        // File should be at {ws_dir}/goals.md, not {ws_dir}/{uuid}/goals.md
        assert!(ws_dir.join("goals.md").exists());
        assert_eq!(ws.read_file(WorkspaceFile::Goals).unwrap(), "Review code");
    }

    #[test]
    fn test_write_and_read_user() {
        let (_dir, ws) = test_workspace();
        ws.write_file(WorkspaceFile::User, "Name: Alper\nStyle: concise")
            .unwrap();
        let content = ws.read_file(WorkspaceFile::User).unwrap();
        assert!(content.contains("Alper"));
    }
}
