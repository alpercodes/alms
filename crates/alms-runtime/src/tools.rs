use alms_core::{AlmsError, AlmsResult};
use alms_sandbox::{FileStateCache, SandboxError, ToolRegistry as SandboxRegistry};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, warn};

/// Runtime tool registry wraps the sandbox registry
#[derive(Debug, Clone)]
pub struct ToolRegistry {
    registry: SandboxRegistry,
    /// Per-run file state cache shared by fs_read, fs_write, and fs_edit.
    file_state_cache: Arc<FileStateCache>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    /// Create empty registry
    pub fn new() -> Self {
        Self {
            registry: SandboxRegistry::new(),
            file_state_cache: Arc::new(FileStateCache::default()),
        }
    }

    /// Create registry with sandbox built-in tools (unrestricted).
    ///
    /// The file state cache is created and attached to `fs_read`, `fs_write`,
    /// and `fs_edit` tools so that writes/edits are guarded by a prior read.
    pub fn with_builtins() -> Self {
        let cache = Arc::new(FileStateCache::default());
        let registry = SandboxRegistry::with_builtin_tools();

        // Wire the file state cache into fs tools (same as the sandboxed path).
        Self::attach_fs_cache_to_registry(&registry, None, &cache, &[]);

        Self {
            registry,
            file_state_cache: cache,
        }
    }

    /// Create registry with sandbox-aware built-in tools.
    ///
    /// `enabled` — when non-empty, only builtins whose name appears in the
    /// list are registered. Empty slice = all builtins enabled.
    ///
    /// The file state cache is created and attached to `fs_read`, `fs_write`,
    /// and `fs_edit` tools so that writes/edits are guarded by a prior read.
    pub fn with_builtins_sandboxed(
        sandbox_root: Option<std::path::PathBuf>,
        shell_unrestricted: bool,
        enabled: &[String],
    ) -> Self {
        let cache = Arc::new(FileStateCache::default());
        let registry = SandboxRegistry::with_builtin_tools_sandboxed(
            sandbox_root.clone(),
            shell_unrestricted,
            enabled,
        );

        // Re-register fs_read/fs_write/fs_edit with the file state cache.
        // The initial registration created them without a cache; now we
        // replace them with cache-aware versions.
        Self::attach_fs_cache_to_registry(&registry, sandbox_root.as_ref(), &cache, enabled);

        Self {
            registry,
            file_state_cache: cache,
        }
    }

    /// Get a reference to the file state cache shared by filesystem tools.
    pub fn file_state_cache(&self) -> &Arc<FileStateCache> {
        &self.file_state_cache
    }

    /// Re-register `fs_read`, `fs_write`, and `fs_edit` with the given cache.
    ///
    /// Used both during initial construction and after `with_workspace()`
    /// re-creates tools sandboxed to the workspace directory.
    fn attach_fs_cache_to_registry(
        registry: &SandboxRegistry,
        sandbox_root: Option<&std::path::PathBuf>,
        cache: &Arc<FileStateCache>,
        enabled: &[String],
    ) {
        let tool_enabled = |name: &str| enabled.is_empty() || enabled.iter().any(|t| t == name);

        if tool_enabled("fs_read") {
            let fs_read = match sandbox_root {
                Some(root) => alms_sandbox::FsReadTool::sandboxed(root.clone()),
                None => alms_sandbox::FsReadTool::new(),
            }
            .with_cache(Arc::clone(cache));
            let _ = registry.register(Arc::new(fs_read));
        }
        if tool_enabled("fs_write") {
            let fs_write = match sandbox_root {
                Some(root) => alms_sandbox::FsWriteTool::sandboxed(root.clone()),
                None => alms_sandbox::FsWriteTool::new(),
            }
            .with_cache(Arc::clone(cache));
            let _ = registry.register(Arc::new(fs_write));
        }
        if tool_enabled("fs_edit") {
            let fs_edit = match sandbox_root {
                Some(root) => alms_sandbox::FsEditTool::sandboxed(root.clone()),
                None => alms_sandbox::FsEditTool::new(),
            }
            .with_cache(Arc::clone(cache));
            let _ = registry.register(Arc::new(fs_edit));
        }
    }

    /// Register a custom tool implementation (e.g. WorkspaceWriteTool).
    pub fn register(&self, tool: std::sync::Arc<dyn alms_sandbox::Tool>) {
        if let Err(e) = self.registry.register(tool) {
            warn!("Failed to register tool: {}", e);
        }
    }

    /// Register a tool under a specific alias name (for backward compatibility).
    pub fn register_arc_as(&self, alias: &str, tool: std::sync::Arc<dyn alms_sandbox::Tool>) {
        if let Err(e) = self.registry.register_as(alias, tool) {
            warn!("Failed to register tool alias '{}': {}", alias, e);
        }
    }

    /// List all tool names
    pub fn list(&self) -> Vec<String> {
        self.registry.list()
    }

    /// Check if tool exists
    pub fn contains(&self, name: &str) -> bool {
        self.registry.contains(name)
    }

    /// Get tool definitions for LLM with real parameter schemas.
    ///
    /// Deduplicates by `tool.name()` so that aliases (e.g. "shell_exec" ->
    /// "shell") do not produce duplicate function definitions — OpenAI and
    /// other providers reject payloads with duplicate function names.
    pub fn to_definitions(&self) -> Vec<crate::llm_types::ToolDefinition> {
        let mut seen = HashSet::new();
        self.registry
            .list()
            .into_iter()
            .filter_map(|name| {
                let tool = self.registry.lookup(&name).ok()?;
                let canonical_name = tool.name().to_string();
                if !seen.insert(canonical_name) {
                    return None;
                }
                Some(
                    crate::llm_types::ToolDefinition::new(tool.name(), tool.description())
                        .with_parameters(tool.parameters()),
                )
            })
            .collect()
    }

    /// Check whether a tool is auto-approved (bypasses approval in guarded posture).
    pub fn is_auto_approved(&self, name: &str) -> bool {
        self.registry
            .lookup(name)
            .map(|t| t.is_auto_approved())
            .unwrap_or(false)
    }

    /// Execute a tool by name via sandbox
    pub async fn execute(&self, name: &str, params: Value) -> AlmsResult<Value> {
        debug!("Executing tool via sandbox: {}", name);
        self.registry
            .execute(name, params)
            .await
            .map_err(|e| match e {
                SandboxError::ToolNotFound(tool) => {
                    AlmsError::ToolExecution(format!("Tool '{}' not found", tool))
                }
                other => {
                    warn!("Tool execution failed: {}", other);
                    AlmsError::ToolExecution(other.to_string())
                }
            })
    }
}
