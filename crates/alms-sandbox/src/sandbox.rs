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
#[derive(Debug)]
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
        wasm_config.static_memory_maximum_size(config.max_memory as u64);
        wasm_config.dynamic_memory_guard_size(64 * 1024); // 64KB guard pages

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
        function_name: &str,
        params: Value,
    ) -> SandboxResult<Value> {
        let start_time = std::time::Instant::now();
        
        info!(
            "Executing WASM module, function: {}, params: {:?}",
            function_name, params
        );

        // Compile the module
        let module = self.compile(wasm_bytes).await?;

        // Create store and instance
        let mut store = Store::new(&self.engine, SandboxState::new(self.config.clone()));
        
        // Add fuel if enabled
        if self.config.fuel_enabled {
            store.add_fuel(self.config.initial_fuel)
                .map_err(|e| SandboxError::WasmExecution(format!("Failed to add fuel: {}", e)))?;
        }

        // Create instance
        let instance = self.instantiate(&mut store, &module).await?;

        // Get the exported function
        let func = self.get_function(&mut store, &instance, function_name)?;

        // Serialize params to JSON
        let params_json = serde_json::to_vec(&params)?;
        let params_len = params_json.len() as i32;

        // Get memory export
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| SandboxError::WasmExecution("Memory export not found".to_string()))?;

        // Allocate space for input and write it
        let ptr = self.allocate(&mut store, &memory, params_len as usize).await?;
        
        memory.write(&mut store, ptr as usize, &params_json)
            .map_err(|e| SandboxError::MemoryLimitExceeded {
                allocated: ptr as usize + params_len as usize,
                limit: self.config.max_memory,
            })?;

        // Call the function
        trace!("Calling WASM function with ptr={}, len={}", ptr, params_len);
        let result_ptr: i32 = func.call_async(&mut store, (ptr, params_len))
            .await
            .map_err(|e| SandboxError::WasmExecution(format!("Function call failed: {}", e)))?;

        // Read result from memory
        // First read 4 bytes for length
        let mut len_bytes = [0u8; 4];
        memory.read(&store, result_ptr as usize, &mut len_bytes)
            .map_err(|e| SandboxError::WasmExecution(format!("Failed to read result length: {}", e)))?;
        let result_len = i32::from_le_bytes(len_bytes) as usize;

        // Validate result length
        if result_len > self.config.max_memory {
            return Err(SandboxError::MemoryLimitExceeded {
                allocated: result_len,
                limit: self.config.max_memory,
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

        // Check for timeout
        if elapsed > self.config.timeout {
            return Err(SandboxError::ExecutionTimeout(self.config.timeout));
        }

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

    /// Allocate memory in the WASM instance
    async fn allocate(
        &self,
        store: &mut Store<SandboxState>,
        memory: &Memory,
        size: usize,
    ) -> SandboxResult<i32> {
        // Simple allocation: use the first available space
        // In a real implementation, you'd want to track allocations
        let current_size = memory.data_size(&*store);
        
        if size > self.config.max_memory {
            return Err(SandboxError::MemoryLimitExceeded {
                allocated: size,
                limit: self.config.max_memory,
            });
        }

        if size > current_size {
            // Grow memory
            let pages_needed = ((size - current_size) + 65535) / 65536;
            memory.grow(&mut *store, pages_needed as u64)
                .map_err(|e| SandboxError::MemoryLimitExceeded {
                    allocated: size,
                    limit: self.config.max_memory,
                })?;
        }

        // Return pointer to start of memory
        Ok(0)
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

    // Simple test WASM module that doubles a number
    // This is a minimal valid WASM module for testing
    // In real usage, you'd compile Rust/C to WASM
    const TEST_WASM: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, // magic
        0x01, 0x00, 0x00, 0x00, // version
        // Minimal module with a simple function
    ];

    #[test]
    fn test_sandbox_config() {
        let config = SandboxConfig::new()
            .with_max_memory(128 * 1024 * 1024)
            .with_timeout(Duration::from_secs(60));

        assert_eq!(config.max_memory, 128 * 1024 * 1024);
        assert_eq!(config.timeout, Duration::from_secs(60));
    }

    #[tokio::test]
    async fn test_sandbox_creation() {
        let config = SandboxConfig::default();
        let sandbox = Sandbox::new(config);
        assert!(sandbox.config.fuel_enabled);
    }
}
