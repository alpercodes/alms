use crate::{SandboxError, error::SandboxResult};
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, error, info, trace, warn};
use wasmtime::{Config, Engine, Module, Store};

/// Configuration for the WASM sandbox
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    /// Maximum memory per instance in bytes (default: 64MB)
    pub max_memory: usize,
    /// Maximum input payload bytes (default: 1 MiB)
    pub max_input_bytes: usize,
    /// Maximum output payload bytes (default: 4 MiB)
    pub max_output_bytes: usize,
    /// Execution timeout (default: 30 seconds)
    pub timeout: Duration,
    /// Enable fuel metering for deterministic execution
    pub fuel_enabled: bool,
    /// Initial fuel units
    pub initial_fuel: u64,
    /// Enable WASI
    pub wasi_enabled: bool,
    /// Enable debug prints
    pub debug: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            max_memory: 64 * 1024 * 1024,      // 64MB
            max_input_bytes: 1024 * 1024,      // 1 MiB
            max_output_bytes: 4 * 1024 * 1024, // 4 MiB
            timeout: Duration::from_secs(30),
            fuel_enabled: true,
            initial_fuel: 10_000_000_000, // 10 billion units
            wasi_enabled: false,
            debug: false,
        }
    }
}

impl SandboxConfig {
    /// Create a new sandbox config with defaults
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum memory
    pub fn with_max_memory(mut self, bytes: usize) -> Self {
        self.max_memory = bytes;
        self
    }

    /// Set maximum input bytes
    pub fn with_max_input_bytes(mut self, bytes: usize) -> Self {
        self.max_input_bytes = bytes;
        self
    }

    /// Set maximum output bytes
    pub fn with_max_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }

    /// Set execution timeout
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Enable/disable fuel metering
    pub fn with_fuel(mut self, enabled: bool) -> Self {
        self.fuel_enabled = enabled;
        self
    }

    /// Set initial fuel
    pub fn with_initial_fuel(mut self, fuel: u64) -> Self {
        self.initial_fuel = fuel;
        self
    }

    /// Enable WASI
    pub fn with_wasi(mut self, enabled: bool) -> Self {
        self.wasi_enabled = enabled;
        self
    }

    /// Enable debug mode
    pub fn with_debug(mut self, debug: bool) -> Self {
        self.debug = debug;
        self
    }
}

/// WASM execution state (host data stored in wasmtime Store).
/// Currently empty — the log callback only needs memory access via Caller.
#[derive(Debug)]
pub(crate) struct SandboxState;

impl SandboxState {
    fn new(_config: SandboxConfig) -> Self {
        Self
    }
}

/// WASM Sandbox for executing tools in isolation.
///
/// Uses `tokio::task::spawn_blocking` so WASM execution runs on a dedicated
/// thread pool thread instead of a wasmtime fiber, avoiding Windows fiber
/// stack overflow issues and keeping the tokio runtime unblocked.
pub struct Sandbox {
    config: SandboxConfig,
    engine: Engine,
}

impl Sandbox {
    /// Create a new sandbox with the given configuration
    pub fn new(config: SandboxConfig) -> Self {
        let mut wasm_config = Config::new();
        wasm_config.wasm_memory64(false);
        wasm_config.memory_guard_size(64 * 1024); // 64KB guard pages

        // Fuel metering for deterministic execution limits
        if config.fuel_enabled {
            wasm_config.consume_fuel(true);
        }

        let engine = Engine::new(&wasm_config).expect("Failed to create WASM engine");

        debug!("Created new WASM sandbox with config: {:?}", config);
        Self { config, engine }
    }

    /// Execute a WASM module.
    ///
    /// The WASM module must export:
    /// - `memory` — linear memory
    /// - `alms_alloc(len: i32) -> i32` — allocates `len` bytes, returns pointer
    /// - `<entrypoint>(ptr: i32, len: i32) -> i32` — tool call; returns pointer to result
    ///
    /// Result layout at the returned pointer:
    /// - 4 bytes little-endian length
    /// - `length` bytes of JSON
    ///
    /// Timeout is enforced via `tokio::time::timeout`; WASM runs on a blocking
    /// thread via `spawn_blocking` so it does not block the async runtime.
    pub async fn execute(
        &self,
        wasm_bytes: &[u8],
        entrypoint: &str,
        tool_name: &str,
        params: Value,
    ) -> SandboxResult<Value> {
        let start_time = std::time::Instant::now();

        info!(
            "Executing WASM module, entrypoint: {}, tool: {}, params: {:?}",
            entrypoint, tool_name, params
        );

        // Short-circuit for zero timeout — no point in starting execution.
        if self.config.timeout.is_zero() {
            return Err(SandboxError::ExecutionTimeout(self.config.timeout));
        }

        // Build ABI envelope and check input size before touching the engine.
        let payload = serde_json::json!({
            "abi": 0,
            "tool": tool_name,
            "params": params,
        });
        let params_json = serde_json::to_vec(&payload)?;
        if params_json.len() > self.config.max_input_bytes {
            return Err(SandboxError::MemoryLimitExceeded {
                allocated: params_json.len(),
                limit: self.config.max_input_bytes,
            });
        }

        // Compile the module (synchronous Cranelift compilation; fast for small tools).
        trace!("Compiling WASM module ({} bytes)", wasm_bytes.len());
        let module = Module::new(&self.engine, wasm_bytes).map_err(|e| {
            error!("WASM compilation failed: {}", e);
            SandboxError::WasmCompile(e.to_string())
        })?;

        // Move everything needed for execution into the blocking closure.
        let engine = self.engine.clone();
        let config = self.config.clone();
        let entrypoint = entrypoint.to_string();
        let timeout = config.timeout;

        let result = tokio::time::timeout(
            timeout,
            tokio::task::spawn_blocking(move || {
                Self::run_sync(engine, module, config, &entrypoint, params_json)
            }),
        )
        .await;

        let elapsed = start_time.elapsed();
        info!("WASM execution completed in {:?}", elapsed);

        match result {
            Err(_elapsed) => Err(SandboxError::ExecutionTimeout(timeout)),
            Ok(Err(join_err)) => Err(SandboxError::WasmExecution(join_err.to_string())),
            Ok(Ok(inner)) => inner,
        }
    }

    /// Synchronous WASM execution — runs on a `spawn_blocking` thread.
    fn run_sync(
        engine: Engine,
        module: Module,
        config: SandboxConfig,
        entrypoint: &str,
        params_json: Vec<u8>,
    ) -> SandboxResult<Value> {
        let params_len = params_json.len() as i32;

        let mut store = Store::new(&engine, SandboxState::new(config.clone()));

        if config.fuel_enabled {
            store
                .set_fuel(config.initial_fuel)
                .map_err(|e| SandboxError::WasmExecution(format!("Failed to set fuel: {}", e)))?;
        }

        // Build the linker with host imports.
        let mut linker = wasmtime::Linker::new(&engine);
        linker
            .func_wrap(
                "env",
                "log",
                |mut caller: wasmtime::Caller<'_, SandboxState>, ptr: i32, len: i32| {
                    let memory = caller
                        .get_export("memory")
                        .and_then(|e| e.into_memory())
                        .expect("memory export");
                    let mut buffer = vec![0u8; len as usize];
                    memory
                        .read(&caller, ptr as usize, &mut buffer)
                        .expect("read memory");
                    debug!("WASM log: {}", String::from_utf8_lossy(&buffer));
                },
            )
            .map_err(|e| SandboxError::WasmInstantiate(e.to_string()))?;

        let instance = linker.instantiate(&mut store, &module).map_err(|e| {
            error!("WASM instantiation failed: {}", e);
            SandboxError::WasmInstantiate(e.to_string())
        })?;

        // Resolve exports.
        let func = instance
            .get_typed_func::<(i32, i32), i32>(&mut store, entrypoint)
            .map_err(|e| {
                warn!("WASM function '{}' not found: {}", entrypoint, e);
                SandboxError::WasmFunctionNotFound(entrypoint.to_string())
            })?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| SandboxError::WasmExecution("Memory export not found".to_string()))?;

        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "alms_alloc")
            .map_err(|_| SandboxError::WasmFunctionNotFound("alms_alloc".to_string()))?;

        // Allocate input buffer inside WASM memory.
        let ptr = alloc
            .call(&mut store, params_len)
            .map_err(|e| SandboxError::WasmExecution(format!("Allocation failed: {}", e)))?;
        if ptr == 0 {
            return Err(SandboxError::MemoryLimitExceeded {
                allocated: params_len as usize,
                limit: config.max_memory,
            });
        }

        memory
            .write(&mut store, ptr as usize, &params_json)
            .map_err(|_| SandboxError::MemoryLimitExceeded {
                allocated: ptr as usize + params_len as usize,
                limit: config.max_memory,
            })?;

        // Call the tool entrypoint.
        trace!("Calling WASM function with ptr={}, len={}", ptr, params_len);
        let result_ptr: i32 = func
            .call(&mut store, (ptr, params_len))
            .map_err(|e| SandboxError::WasmExecution(format!("Function call failed: {}", e)))?;

        // Read result: 4-byte LE length prefix followed by JSON bytes.
        let mut len_bytes = [0u8; 4];
        memory
            .read(&store, result_ptr as usize, &mut len_bytes)
            .map_err(|e| {
                SandboxError::WasmExecution(format!("Failed to read result length: {}", e))
            })?;
        let result_len = i32::from_le_bytes(len_bytes) as usize;

        if result_len > config.max_output_bytes {
            return Err(SandboxError::MemoryLimitExceeded {
                allocated: result_len,
                limit: config.max_output_bytes,
            });
        }

        let mut result_bytes = vec![0u8; result_len];
        memory
            .read(&store, (result_ptr + 4) as usize, &mut result_bytes)
            .map_err(|e| SandboxError::WasmExecution(format!("Failed to read result: {}", e)))?;

        serde_json::from_slice(&result_bytes)
            .map_err(|e| SandboxError::InvalidResult(format!("Failed to parse result: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wasm_ok() -> Vec<u8> {
        let wat = r#"
            (module
              (memory (export "memory") 1)
              (global $heap (mut i32) (i32.const 64))
              (func (export "alms_alloc") (param $len i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $heap))
                (global.set $heap (i32.add (global.get $heap) (local.get $len)))
                (local.get $ptr)
              )
              (data (i32.const 0) "\22\00\00\00{\"ok\":true,\"result\":{\"echo\":\"hi\"}}")
              (func (export "alms_tool_call") (param $ptr i32) (param $len i32) (result i32)
                (i32.const 0)
              )
            )
        "#;
        wat::parse_str(wat).expect("valid wat")
    }

    fn wasm_bad_json() -> Vec<u8> {
        let wat = r#"
            (module
              (memory (export "memory") 1)
              (global $heap (mut i32) (i32.const 64))
              (func (export "alms_alloc") (param $len i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $heap))
                (global.set $heap (i32.add (global.get $heap) (local.get $len)))
                (local.get $ptr)
              )
              (data (i32.const 0) "\05\00\00\00oops!")
              (func (export "alms_tool_call") (param $ptr i32) (param $len i32) (result i32)
                (i32.const 0)
              )
            )
        "#;
        wat::parse_str(wat).expect("valid wat")
    }

    #[tokio::test]
    async fn test_execute_ok() {
        let sandbox = Sandbox::new(SandboxConfig::default());
        let result = sandbox
            .execute(
                &wasm_ok(),
                "alms_tool_call",
                "echo",
                serde_json::json!({"x": 1}),
            )
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    #[tokio::test]
    async fn test_input_size_limit() {
        let sandbox = Sandbox::new(SandboxConfig::default().with_max_input_bytes(10));
        let err = sandbox
            .execute(
                &wasm_ok(),
                "alms_tool_call",
                "echo",
                serde_json::json!({"x": "too_large_payload"}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::MemoryLimitExceeded { .. }));
    }

    #[tokio::test]
    async fn test_output_size_limit() {
        let sandbox = Sandbox::new(SandboxConfig::default().with_max_output_bytes(8));
        let err = sandbox
            .execute(
                &wasm_ok(),
                "alms_tool_call",
                "echo",
                serde_json::json!({"x": 1}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::MemoryLimitExceeded { .. }));
    }

    #[tokio::test]
    async fn test_invalid_json_output() {
        let sandbox = Sandbox::new(SandboxConfig::default());
        let err = sandbox
            .execute(
                &wasm_bad_json(),
                "alms_tool_call",
                "echo",
                serde_json::json!({"x": 1}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidResult(_)));
    }

    #[tokio::test]
    async fn test_timeout() {
        let sandbox = Sandbox::new(SandboxConfig::default().with_timeout(Duration::from_millis(0)));
        let err = sandbox
            .execute(
                &wasm_ok(),
                "alms_tool_call",
                "echo",
                serde_json::json!({"x": 1}),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::ExecutionTimeout(_)));
    }
}
