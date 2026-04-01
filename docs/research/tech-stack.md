# Tech Stack Decision: Rust vs Go for ALMS

## Research Summary

### Go Strengths
- **Concurrency**: Goroutines are extremely lightweight, channels are built-in
- **Developer Velocity**: Small language, easy to learn, fast to write
- **Microservices**: Excellent for cloud-native distributed systems
- **Ecosystem**: Mature HTTP/gRPC libraries, great for APIs
- **Simplicity**: Easy to maintain, even junior devs can contribute

### Go Weaknesses
- **Memory Safety**: No borrow checker, GC pauses possible
- **Performance**: Good but not Rust-level
- **WASM Support**: Less mature than Rust for plugin system

### Rust Strengths
- **Memory Safety**: Zero-cost abstractions, no GC
- **Performance**: Maximum throughput, predictable latency
- **WASM**: First-class support for sandboxed plugins
- **Reliability**: Compile-time guarantees prevent entire classes of bugs
- **Concurrency**: Fearless parallelism with ownership system

### Rust Weaknesses
- **Learning Curve**: Steeper, borrow checker fights initially
- **Development Speed**: Slower to write, especially for junior devs
- **Complexity**: Rich type system can be overwhelming

## Recommendation: Rust

For ALMS specifically:

1. **Agent Loops Need Predictable Performance** - No GC pauses during critical agent operations
2. **Security is Paramount** - Rust's memory safety prevents entire exploit classes
3. **WASM Plugins** - Rust has best-in-class WASM tooling (wasmtime, wasmer)
4. **Long-term Maintenance** - ALMS is infrastructure, not a CRUD app. Correctness > velocity.
5. **Concurrency Model** - Agent loops need fine-grained control over execution, Rust gives this

## Hybrid Approach

- **Core Services (Rust)**: Gateway, Session Manager, Agent Runtime, Tool Sandbox
- **SDK/CLI (Go)**: Developer tooling, CLI, integration libraries
- **Plugins (WASM)**: User extensions, sandboxed execution

## Decision

**Primary stack: Rust**

Key crates:
- `tokio` - Async runtime
- `axum` - HTTP/WebSocket server
- `tonic` - gRPC
- `wasmtime` - WASM runtime for plugins
- `serde` - Serialization
- `parking_lot` - Fast synchronization primitives
- `dashmap` - Concurrent hashmap for sessions

---
Research Date: 2026-02-09