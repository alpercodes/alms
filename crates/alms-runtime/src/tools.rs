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
        // fuzzy_match defaults to off at this layer; callers that want it on
        // use `with_builtins_sandboxed_ex`.
        Self::attach_fs_cache_to_registry(&registry, None, &cache, &[], false);

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
        // Back-compat shim: forward to the extended variant with
        // fs_edit_fuzzy_match=false (the issue #755 default).
        Self::with_builtins_sandboxed_ex(sandbox_root, shell_unrestricted, enabled, false)
    }

    /// Like [`Self::with_builtins_sandboxed`], but also takes the per-agent
    /// `fs_edit.fuzzy_match` flag (issue #755). When `true`, the
    /// `FsEditTool` registered here runs the additional line-trimmed +
    /// indentation-flexible replacer stages before the curly-quote /
    /// CRLF fallback. Default (`false`) preserves the exact-string contract.
    pub fn with_builtins_sandboxed_ex(
        sandbox_root: Option<std::path::PathBuf>,
        shell_unrestricted: bool,
        enabled: &[String],
        fs_edit_fuzzy_match: bool,
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
        Self::attach_fs_cache_to_registry(
            &registry,
            sandbox_root.as_ref(),
            &cache,
            enabled,
            fs_edit_fuzzy_match,
        );

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
    ///
    /// `fs_edit_fuzzy_match` is baked into the `FsEditTool` at construction
    /// time (config-file-only per issue #755).
    fn attach_fs_cache_to_registry(
        registry: &SandboxRegistry,
        sandbox_root: Option<&std::path::PathBuf>,
        cache: &Arc<FileStateCache>,
        _enabled: &[String],
        fs_edit_fuzzy_match: bool,
    ) {
        // No pre-filtering by `enabled` here — `registry.register()` already
        // checks its internal `enabled_filter` and skips disabled tools.
        let fs_read = match sandbox_root {
            Some(root) => alms_sandbox::FsReadTool::sandboxed(root.clone()),
            None => alms_sandbox::FsReadTool::new(),
        }
        .with_cache(Arc::clone(cache));
        let _ = registry.register(Arc::new(fs_read));

        let fs_write = match sandbox_root {
            Some(root) => alms_sandbox::FsWriteTool::sandboxed(root.clone()),
            None => alms_sandbox::FsWriteTool::new(),
        }
        .with_cache(Arc::clone(cache));
        let _ = registry.register(Arc::new(fs_write));

        let fs_edit = match sandbox_root {
            Some(root) => alms_sandbox::FsEditTool::sandboxed(root.clone()),
            None => alms_sandbox::FsEditTool::new(),
        }
        .with_cache(Arc::clone(cache))
        .with_fuzzy_match(fs_edit_fuzzy_match);
        let _ = registry.register(Arc::new(fs_edit));
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
    ///
    /// **Stable ordering**: the underlying registry is a `DashMap` whose
    /// iteration order is non-deterministic. Anthropic prompt caching
    /// (#766) requires the tools array to serialize byte-identically
    /// across turns for cache hits, so we sort the output by canonical
    /// tool name. The sort is cheap (tool counts are small, O(n log n)
    /// on <50 entries) and not observable to the LLM beyond the cache
    /// win — tool ordering has no effect on tool selection.
    pub fn to_definitions(&self) -> Vec<crate::llm_types::ToolDefinition> {
        let mut seen = HashSet::new();
        let mut defs: Vec<_> = self
            .registry
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
            .collect();
        defs.sort_by(|a, b| a.function.name.cmp(&b.function.name));
        defs
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
                // Preserve the structured classifier target (#758) so the
                // runtime can include it in SSE tool_end payloads and audit
                // events without regexing the error message.
                SandboxError::ShellBlocked { reason, target } => {
                    warn!("Tool execution blocked by classifier: {reason} (target={target:?})");
                    AlmsError::ToolBlocked { reason, target }
                }
                other => {
                    warn!("Tool execution failed: {}", other);
                    AlmsError::ToolExecution(other.to_string())
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #766 correctness: `to_definitions()` must return the tools
    /// array in a deterministic, name-sorted order. The underlying
    /// registry is a `DashMap` whose iteration order is arbitrary;
    /// Anthropic prompt caching requires the tools wire shape to be
    /// byte-identical across turns for cache hits.
    #[test]
    fn test_to_definitions_is_sorted_by_name() {
        // Use the builtin registry — it provides a representative set
        // (>10 tools) without requiring custom Tool fixtures.
        let reg = ToolRegistry::with_builtins();
        let defs = reg.to_definitions();

        let names: Vec<&str> = defs.iter().map(|d| d.function.name.as_str()).collect();
        assert!(
            names.len() >= 2,
            "builtin registry must have at least two tools for this test to be meaningful"
        );

        // Sorted — adjacent entries must be in ascending order.
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(
            names, sorted,
            "to_definitions() must return tools in sorted name order for cache-stability"
        );

        // Deterministic — two successive calls produce the same output.
        let defs_again = reg.to_definitions();
        let names_again: Vec<&str> = defs_again
            .iter()
            .map(|d| d.function.name.as_str())
            .collect();
        assert_eq!(
            names, names_again,
            "to_definitions() must produce the same ordering across calls"
        );
    }
}
