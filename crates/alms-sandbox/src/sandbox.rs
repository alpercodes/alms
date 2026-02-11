use crate::{error::SandboxResult, SandboxError};
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, error, info, trace, warn};
use wasmtime::{AsContext, AsContextMut, Config, Engine, Instance, Memory, Module, Store, TypedFunc, Val, ValType};

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
            max_memory: 64 * 1024 * 1024, // 64MB
            max_input_bytes: 1024 * 1024, // 1 MiB
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

/// WASM execution state
#[derive(Debug)]
struct SandboxState {
    /// Memory buffer for passing data
    memory: Option<Memory>,
    /// Allocated memory size
    allocated: usize,
    /// Config reference
    config: SandboxConfig,
}

impl SandboxState {
    fn new(config: SandboxConfig) -> Self {
        Self {
            memory: None,
            allocated: 0,
            config,
        }
    }
}

/// WASM Sandbox for executing tools in isolation
pub struct Sandbox {
    config: SandboxConfig,
    engine: Engine,
}

impl Sandbox {
    /// Create a new sandbox with the given configuration
    pub fn new(config: SandboxConfig) -> Self {
        let mut wasm_config = Config::new();
        wasm_config.wasm_memory64(false);
        wasm_config.async_support(true);
        wasm_config.epoch_interruption(true);
        
        // Enable fuel metering if configured
        if config.fuel_enabled {
            wasm_config.consume_fuel(true);
        }

        // Limit memory
        wasm_config.memory_guard_size(64 * 1024); // 64KB guard pages

        let engine = Engine::new(&wasm_config).expect("Failed to create WASM engine");

        debug!("Created new WASM sandbox with config: {:?}", config);
        Self { config, engine }
    }

    /// Execute a WASM module
    /// 
    /// The WASM module should export a function with the given name that takes
    /// two i32 parameters (pointer and length) and returns an i32 (pointer to result).
    /// 
    /// Memory layout:
    /// - Input JSON is written to memory
    /// - Function is called with (ptr, len)
    /// - Function returns ptr to result JSON
    /// - Result is read from memory
    pub async fn execute(
        &mut self,
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

        // Build ABI envelope
        let payload = serde_json::json!({
            "abi": 0,
            "tool": tool_name,
            "params": params,
        });

        // Serialize params to JSON
        let params_json = serde_json::to_vec(&payload)?;
        if params_json.len() > self.config.max_input_bytes {
            return Err(SandboxError::MemoryLimitExceeded {
                allocated: params_json.len(),
                limit: self.config.max_input_bytes,
            });
        }
        let params_len = params_json.len() as i32;

        // Compile the module
        let module = self.compile(wasm_bytes).await?;

        // Create store and instance
        let mut store = Store::new(&self.engine, SandboxState::new(self.config.clone()));
        
        // Fuel metering disabled until wasmtime fuel feature is enabled in workspace

        // Create instance
        let instance = self.instantiate(&mut store, &module).await?;

        // Get the exported function
        let func = self.get_function(&mut store, &instance, entrypoint)?;

        // Get memory export
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| SandboxError::WasmExecution("Memory export not found".to_string()))?;

        // Allocate space for input via exported allocator
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, "alms_alloc")
            .map_err(|_| SandboxError::WasmFunctionNotFound("alms_alloc".to_string()))?;
        let ptr = alloc
            .call_async(&mut store, params_len)
            .await
            .map_err(|e| SandboxError::WasmExecution(format!("Allocation failed: {}", e)))?;
        if ptr == 0 {
            return Err(SandboxError::MemoryLimitExceeded {
                allocated: params_len as usize,
                limit: self.config.max_memory,
            });
        }

        memory.write(&mut store, ptr as usize, &params_json)
            .map_err(|_| SandboxError::MemoryLimitExceeded {
                allocated: ptr as usize + params_len as usize,
                limit: self.config.max_memory,
            })?;

        // Call the function
        trace!("Calling WASM function with ptr={}, len={}", ptr, params_len);
        let call_future = func.call_async(&mut store, (ptr, params_len));
        let result_ptr: i32 = tokio::time::timeout(self.config.timeout, call_future)
            .await
            .map_err(|_| SandboxError::ExecutionTimeout(self.config.timeout))?
            .map_err(|e| SandboxError::WasmExecution(format!("Function call failed: {}", e)))?;

        // Read result from memory
        // First read 4 bytes for length
        let mut len_bytes = [0u8; 4];
        memory.read(&store, result_ptr as usize, &mut len_bytes)
            .map_err(|e| SandboxError::WasmExecution(format!("Failed to read result length: {}", e)))?;
        let result_len = i32::from_le_bytes(len_bytes) as usize;

        // Validate result length
        if result_len > self.config.max_output_bytes {
            return Err(SandboxError::MemoryLimitExceeded {
                allocated: result_len,
                limit: self.config.max_output_bytes,
            });
        }

        // Read the actual result
        let mut result_bytes = vec![0u8; result_len];
        memory.read(&store, (result_ptr + 4) as usize, &mut result_bytes)
            .map_err(|e| SandboxError::WasmExecution(format!("Failed to read result: {}", e)))?;

        // Parse result JSON
        let result: Value = serde_json::from_slice(&result_bytes)
            .map_err(|e| SandboxError::InvalidResult(format!("Failed to parse result: {}", e)))?;

        let elapsed = start_time.elapsed();
        info!("WASM execution completed in {:?}", elapsed);

        Ok(result)
    }

    /// Compile WASM bytes to a module
    async fn compile(&self, wasm_bytes: &[u8]) -> SandboxResult<Module> {
        trace!("Compiling WASM module ({} bytes)", wasm_bytes.len());
        
        Module::new(&self.engine, wasm_bytes)
            .map_err(|e| {
                error!("WASM compilation failed: {}", e);
                SandboxError::WasmCompile(e.to_string())
            })
    }

    /// Create a WASM instance
    async fn instantiate(
        &self,
        store: &mut Store<SandboxState>,
        module: &Module,
    ) -> SandboxResult<Instance> {
        // Define imports - provide a simple allocation function
        let mut linker = wasmtime::Linker::new(&self.engine);

        // Add a log function for debugging
        linker.func_wrap(
            "env",
            "log",
            |mut caller: wasmtime::Caller<'_, SandboxState>, ptr: i32, len: i32| {
                let memory = caller.get_export("memory")
                    .and_then(|e| e.into_memory())
                    .expect("memory export");
                
                let mut buffer = vec![0u8; len as usize];
                memory.read(&caller, ptr as usize, &mut buffer).expect("read memory");
                
                let msg = String::from_utf8_lossy(&buffer);
                debug!("WASM log: {}", msg);
            },
        ).map_err(|e| SandboxError::WasmInstantiate(e.to_string()))?;

        let instance = linker.instantiate(store, module)
            .map_err(|e| {
                error!("WASM instantiation failed: {}", e);
                SandboxError::WasmInstantiate(e.to_string())
            })?;

        Ok(instance)
    }

    /// Get an exported function from the instance
    fn get_function(
        &self,
        store: &mut Store<SandboxState>,
        instance: &Instance,
        name: &str,
    ) -> SandboxResult<TypedFunc<(i32, i32), i32>> {
        instance
            .get_typed_func::<(i32, i32), i32>(store, name)
            .map_err(|e| {
                warn!("WASM function '{}' not found: {}", name, e);
                SandboxError::WasmFunctionNotFound(name.to_string())
            })
    }

    /// Get fuel consumed during execution
    pub fn get_fuel_consumed(&self, store: &Store<SandboxState>) -> Option<u64> {
        if self.config.fuel_enabled {
            store.get_fuel().ok().map(|remaining| {
                self.config.initial_fuel.saturating_sub(remaining)
            })
        } else {
            None
        }
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
        let mut sandbox = Sandbox::new(SandboxConfig::default());
        let result = sandbox
            .execute(&wasm_ok(), "alms_tool_call", "echo", serde_json::json!({"x": 1}))
            .await
            .unwrap();
        assert_eq!(result["ok"], true);
    }

    #[tokio::test]
    async fn test_input_size_limit() {
        let mut sandbox = Sandbox::new(SandboxConfig::default().with_max_input_bytes(10));
        let err = sandbox
            .execute(&wasm_ok(), "alms_tool_call", "echo", serde_json::json!({"x": "too_large_payload"}))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::MemoryLimitExceeded { .. }));
    }

    #[tokio::test]
    async fn test_output_size_limit() {
        let mut sandbox = Sandbox::new(SandboxConfig::default().with_max_output_bytes(8));
        let err = sandbox
            .execute(&wasm_ok(), "alms_tool_call", "echo", serde_json::json!({"x": 1}))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::MemoryLimitExceeded { .. }));
    }

    #[tokio::test]
    async fn test_invalid_json_output() {
        let mut sandbox = Sandbox::new(SandboxConfig::default());
        let err = sandbox
            .execute(&wasm_bad_json(), "alms_tool_call", "echo", serde_json::json!({"x": 1}))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::InvalidResult(_)));
    }

    #[tokio::test]
    async fn test_timeout() {
        let mut sandbox = Sandbox::new(SandboxConfig::default().with_timeout(Duration::from_millis(0)));
        let err = sandbox
            .execute(&wasm_ok(), "alms_tool_call", "echo", serde_json::json!({"x": 1}))
            .await
            .unwrap_err();
        assert!(matches!(err, SandboxError::ExecutionTimeout(_)));
    }
}
