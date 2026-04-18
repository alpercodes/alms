.PHONY: all fmt clippy test check ci

# Default target - run all checks
all: fmt clippy test

# Format code
fmt:
	@echo "==> Formatting code"
	cargo fmt --all

# Check formatting without modifying
fmt-check:
	@echo "==> Checking formatting"
	cargo fmt --all -- --check

# Run clippy
clippy:
	@echo "==> Running clippy"
	cargo clippy --all-targets --all-features -- -D warnings

# Run all tests
test:
	@echo "==> Running tests"
	cargo test --all

# Run golden tests specifically
test-golden:
	@echo "==> Running golden tests"
	cargo test --package alms-gateway --test sse_golden_tests

# Run checks without building (fast)
check:
	@echo "==> Running cargo check"
	cargo check --all

# Full CI pipeline (what CI runs)
ci: fmt-check clippy test build-release
	@echo "==> All CI checks passed"

# Build release binary (CI includes this)
build-release:
	@echo "==> Building release binary"
	cargo build --release

# Build release binary
build:
	@echo "==> Building release binary"
	cargo build --release

# Clean build artifacts
clean:
	@echo "==> Cleaning build artifacts"
	cargo clean

# Update dependencies
update:
	@echo "==> Updating dependencies"
	cargo update

# Run the gateway locally (for testing)
run:
	@echo "==> Running ALMS gateway"
	cargo run --package alms-cli -- gateway --bind 127.0.0.1:8080
