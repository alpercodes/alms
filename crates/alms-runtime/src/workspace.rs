//! Agent workspace — persistent identity files.
//!
//! Each agent has a workspace directory containing:
//! - personality.md — tone, style, constraints (user-editable)
//! - goals.md — current objectives (agent + user editable)
//! - memories.md — learned facts, preferences (agent + user editable)
//!
//! These are read at the start of each run and injected into the system prompt.
//! The agent can update goals.md and memories.md via the workspace_write tool.

use alms_core::AgentId;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Agent workspace — reads and manages persistent agent identity files.
#[derive(Debug, Clone)]
pub struct AgentWorkspace {
    /// Root directory for all agent workspaces
    base_dir: PathBuf,
    /// This agent's ID
    agent_id: AgentId,
}

/// Files that can be read/written in the workspace
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceFile {
    Personality,
    Goals,
    Memories,
}

impl WorkspaceFile {
    pub fn filename(&self) -> &str {
        match self {
            WorkspaceFile::Personality => "personality.md",
            WorkspaceFile::Goals => "goals.md",
            WorkspaceFile::Memories => "memories.md",
        }
    }

    /// Whether the agent is allowed to write this file
    pub fn agent_writable(&self) -> bool {
        match self {
            WorkspaceFile::Personality => false, // user-only
            WorkspaceFile::Goals => true,
            WorkspaceFile::Memories => true,
        }
    }

    pub fn all() -> &'static [WorkspaceFile] {
        &[
            WorkspaceFile::Personality,
            WorkspaceFile::Goals,
            WorkspaceFile::Memories,
        ]
    }
}

impl AgentWorkspace {
    pub fn new(base_dir: impl Into<PathBuf>, agent_id: AgentId) -> Self {
        Self {
            base_dir: base_dir.into(),
            agent_id,
        }
    }

    /// Get the workspace directory for this agent
    pub fn dir(&self) -> PathBuf {
        self.base_dir.join(self.agent_id.0.to_string())
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
            Ok(_) => None, // empty file
            Err(_) => None, // doesn't exist
        }
    }

    /// Write a workspace file. Only goals.md and memories.md are agent-writable.
    pub fn write_file(&self, file: WorkspaceFile, content: &str) -> Result<(), String> {
        if !file.agent_writable() {
            return Err(format!(
                "{} is not agent-writable (edit it manually)",
                file.filename()
            ));
        }

        if let Err(e) = self.ensure_dir() {
            return Err(format!("Cannot create workspace dir: {}", e));
        }

        let path = self.dir().join(file.filename());
        std::fs::write(&path, content)
            .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;

        info!("Updated workspace file: {}", path.display());
        Ok(())
    }

    /// Append to a workspace file (for memories).
    pub fn append_file(&self, file: WorkspaceFile, content: &str) -> Result<(), String> {
        if !file.agent_writable() {
            return Err(format!(
                "{} is not agent-writable",
                file.filename()
            ));
        }

        if let Err(e) = self.ensure_dir() {
            return Err(format!("Cannot create workspace dir: {}", e));
        }

        let path = self.dir().join(file.filename());
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let new_content = if existing.is_empty() {
            content.to_string()
        } else {
            format!("{}\n{}", existing.trim_end(), content)
        };

        std::fs::write(&path, new_content)
            .map_err(|e| format!("Failed to append to {}: {}", path.display(), e))?;

        info!("Appended to workspace file: {}", path.display());
        Ok(())
    }

    /// Check if this is a fresh agent (no workspace files exist)
    pub fn needs_bootstrap(&self) -> bool {
        // Bootstrap if personality.md doesn't exist
        self.read_file(WorkspaceFile::Personality).is_none()
    }

    /// Build system prompt prefix from workspace files.
    /// Returns the assembled text to prepend to the system prompt.
    pub fn build_system_prompt_prefix(&self) -> String {
        let mut parts = Vec::new();

        if let Some(personality) = self.read_file(WorkspaceFile::Personality) {
            parts.push(personality);
        }

        if let Some(goals) = self.read_file(WorkspaceFile::Goals) {
            parts.push(format!("## Current Goals\n{}", goals));
        }

        if let Some(memories) = self.read_file(WorkspaceFile::Memories) {
            // Truncate memories if too long (will be properly budgeted by ContextBuilder)
            let memories = if memories.len() > 4000 {
                format!(
                    "{}...\n[memories truncated, {} chars total]",
                    &memories[..4000],
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
        r#"You are a new ALMS agent being set up for the first time. You need to learn about your purpose and the user's preferences.

Ask the user the following (naturally, in conversation — not as a rigid list):
1. What is your primary purpose/role?
2. How should you communicate? (formal/casual, concise/detailed, etc.)
3. What name should you use for the user?
4. Any specific constraints or things to avoid?

After gathering this information, use the workspace_write tool to save:
- personality.md: Your personality, tone, role, and constraints
- goals.md: Your initial objectives based on what the user described

Keep files concise — a few paragraphs at most. You can always update them later."#
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_workspace() -> (TempDir, AgentWorkspace) {
        let dir = TempDir::new().unwrap();
        let ws = AgentWorkspace::new(dir.path(), AgentId::new());
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
        ws.write_file(WorkspaceFile::Goals, "Build the thing").unwrap();
        assert_eq!(
            ws.read_file(WorkspaceFile::Goals).unwrap(),
            "Build the thing"
        );
    }

    #[test]
    fn test_personality_not_writable() {
        let (_dir, ws) = test_workspace();
        let result = ws.write_file(WorkspaceFile::Personality, "anything");
        assert!(result.is_err());
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
        assert!(ws.build_system_prompt_prefix().is_empty());
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
        ws.write_file(WorkspaceFile::Goals, "Help with Rust").unwrap();

        let prefix = ws.build_system_prompt_prefix();
        assert!(prefix.contains("concise and technical"));
        assert!(prefix.contains("Help with Rust"));
    }
}
