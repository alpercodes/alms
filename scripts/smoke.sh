#!/usr/bin/env bash
# smoke.sh — End-to-end smoke test for ALMS gateway
#
# Usage:
#   ./scripts/smoke.sh              # Uses mock LLM (no API key needed)
#   ./scripts/smoke.sh --real       # Uses real LLM (needs OPENROUTER_API_KEY)
#
# What it tests:
#   1. Health endpoint
#   2. Session creation
#   3. POST /runs (canonical async API)
#   4. GET /runs/:id (status polling)
#   5. GET /runs/:id/events (SSE stream)
#   6. Tool execution via /runs (real LLM only)

set -uo pipefail

BIND="127.0.0.1:18080"
BASE="http://${BIND}"
BINARY=""
PID=""
PASS=0
FAIL=0
REAL_LLM=false

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m'

cleanup() {
    if [ -n "$PID" ] && kill -0 "$PID" 2>/dev/null; then
        echo -e "\n${YELLOW}Stopping ALMS (pid $PID)...${NC}"
        kill "$PID" 2>/dev/null || true
        wait "$PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

log_pass() {
    echo -e "  ${GREEN}✓${NC} $1"
    PASS=$((PASS + 1))
}

log_fail() {
    echo -e "  ${RED}✗${NC} $1: $2"
    FAIL=$((FAIL + 1))
}

json_field() {
    python3 -c "import sys,json; print(json.load(sys.stdin).get('$1',''))" 2>/dev/null <<< "$2"
}

# Generate a v4 UUID (works on Linux with python3)
gen_uuid() {
    python3 -c "import uuid; print(uuid.uuid4())"
}

# Parse args
for arg in "$@"; do
    case "$arg" in
        --real) REAL_LLM=true ;;
        *) echo "Unknown arg: $arg"; exit 1 ;;
    esac
done

# Find binary (prefer release, fall back to debug)
for candidate in "./target/release/alms" "./target/debug/alms"; do
    if [ -f "$candidate" ]; then
        BINARY="$candidate"
        break
    fi
done

if [ -z "$BINARY" ]; then
    echo -e "${RED}Error: No ALMS binary found. Run 'cargo build' first.${NC}"
    exit 1
fi

echo "=== ALMS Smoke Test ==="
echo "Binary: $BINARY"

# Set up environment
if [ "$REAL_LLM" = true ]; then
    if [ -z "${OPENROUTER_API_KEY:-}" ]; then
        echo -e "${RED}Error: --real requires OPENROUTER_API_KEY${NC}"
        exit 1
    fi
    echo "Mode: Real LLM (${DEFAULT_MODEL:-openrouter/moonshotai/kimi-k2.5})"
else
    export ALMS_LLM_MOCK=1
    echo "Mode: Mock LLM"
fi

# Start server
echo -e "\nStarting ALMS gateway on ${BIND}..."
$BINARY gateway --bind "$BIND" 2>/dev/null &
PID=$!

# Wait for server to be ready (up to 15s)
for i in $(seq 1 30); do
    if curl -sf "${BASE}/health" > /dev/null 2>&1; then
        break
    fi
    if ! kill -0 "$PID" 2>/dev/null; then
        echo -e "${RED}Server exited prematurely${NC}"
        exit 1
    fi
    sleep 0.5
done

if ! curl -sf "${BASE}/health" > /dev/null 2>&1; then
    echo -e "${RED}Server failed to start within 15s${NC}"
    exit 1
fi

echo -e "Server ready (pid $PID)\n"

# ============================================================
# Test 1: Health check
# ============================================================
echo "Test 1: Health endpoint"
HEALTH=$(curl -s "${BASE}/health")
if echo "$HEALTH" | grep -q '"status":"healthy"'; then
    VERSION=$(json_field "version" "$HEALTH")
    log_pass "GET /health → healthy (v${VERSION})"
else
    log_fail "GET /health" "$HEALTH"
fi

# ============================================================
# Test 2: Session creation
# ============================================================
echo "Test 2: Session creation"
AGENT_UUID=$(gen_uuid)
SESSION=$(curl -s -X POST "${BASE}/sessions" \
    -H "Content-Type: application/json" \
    -d "{\"agent_id\":\"${AGENT_UUID}\",\"context_id\":\"smoke-ctx-1\"}")
SESSION_ID=$(json_field "session_id" "$SESSION")
if [ -n "$SESSION_ID" ]; then
    log_pass "POST /sessions → session_id=${SESSION_ID}"
else
    log_fail "POST /sessions" "$SESSION"
fi

# ============================================================
# Test 3: POST /runs (canonical API)
# ============================================================
echo "Test 3: Canonical /runs endpoint"
if [ -n "$SESSION_ID" ]; then
    RUN_CREATE=$(curl -s -X POST "${BASE}/runs" \
        -H "Content-Type: application/json" \
        -d "{\"session_id\":\"${SESSION_ID}\",\"input\":{\"type\":\"text\",\"text\":\"What is 2+2? Use the math tool.\"}}" \
        --max-time 30)
    RUN_ID=$(json_field "run_id" "$RUN_CREATE")
    RUN_STATUS=$(json_field "status" "$RUN_CREATE")
    if [ -n "$RUN_ID" ]; then
        log_pass "POST /runs → run_id=${RUN_ID} status=${RUN_STATUS}"
    else
        log_fail "POST /runs" "$RUN_CREATE"
    fi
else
    log_fail "POST /runs" "skipped — no session_id from test 2"
fi

# ============================================================
# Test 4: GET /runs/:id (poll for completion)
# ============================================================
echo "Test 4: Run status polling"
if [ -n "${RUN_ID:-}" ]; then
    # Poll up to 10s for completion
    for i in $(seq 1 20); do
        sleep 0.5
        STATUS_RESP=$(curl -s "${BASE}/runs/${RUN_ID}" --max-time 5)
        CUR_STATUS=$(json_field "status" "$STATUS_RESP")
        if [ "$CUR_STATUS" = "completed" ] || [ "$CUR_STATUS" = "failed" ]; then
            break
        fi
    done

    if [ "$CUR_STATUS" = "completed" ]; then
        log_pass "GET /runs/:id → status=completed"
    elif [ "$CUR_STATUS" = "failed" ]; then
        log_fail "Run completed with failure" "$STATUS_RESP"
    else
        log_fail "Run did not complete within 10s" "status=$CUR_STATUS"
    fi
else
    echo "  (skipped — no run_id)"
fi

# ============================================================
# Test 5: SSE event stream (replay via Last-Event-ID)
# ============================================================
echo "Test 5: SSE event stream"
if [ -n "${RUN_ID:-}" ]; then
    SSE_RESULT=$(curl -s "${BASE}/runs/${RUN_ID}/events" \
        -H "Accept: text/event-stream" \
        --max-time 3 || true)
    if [ -n "$SSE_RESULT" ]; then
        EVENT_COUNT=$(echo "$SSE_RESULT" | grep -c "^event:" || true)
        log_pass "GET /runs/:id/events → ${EVENT_COUNT} events"
    else
        log_fail "GET /runs/:id/events" "empty response"
    fi
else
    echo "  (skipped — no run_id)"
fi

# ============================================================
# Test 6: Tool execution via canonical /runs
# ============================================================
echo "Test 6: Tool execution (math builtin)"
if [ "$REAL_LLM" = true ] && [ -n "${SESSION_ID:-}" ]; then
    TOOL_RUN=$(curl -s -X POST "${BASE}/runs" \
        -H "Content-Type: application/json" \
        -d "{\"session_id\":\"${SESSION_ID}\",\"input\":{\"type\":\"text\",\"text\":\"Use the math tool to calculate 15 * 7\"}}" \
        --max-time 30)
    TOOL_RUN_ID=$(json_field "run_id" "$TOOL_RUN")
    if [ -n "$TOOL_RUN_ID" ]; then
        # Poll up to 60s for completion (tool calls take longer)
        TOOL_STATUS=""
        for i in $(seq 1 120); do
            sleep 0.5
            TOOL_STATUS_RESP=$(curl -s "${BASE}/runs/${TOOL_RUN_ID}" --max-time 5)
            TOOL_STATUS=$(json_field "status" "$TOOL_STATUS_RESP")
            if [ "$TOOL_STATUS" = "completed" ] || [ "$TOOL_STATUS" = "failed" ]; then
                break
            fi
        done
        if [ "$TOOL_STATUS" = "completed" ]; then
            log_pass "Tool execution: math run completed"
        else
            log_fail "Tool execution" "status=$TOOL_STATUS"
        fi
    else
        log_fail "Tool execution" "$TOOL_RUN"
    fi
elif [ "$REAL_LLM" != true ]; then
    log_pass "Tool execution: skipped in mock mode (mock doesn't trigger tool calls)"
else
    echo "  (skipped — no session_id)"
fi

# ============================================================
# Summary
# ============================================================
echo ""
echo "=== Results ==="
TOTAL=$((PASS + FAIL))
echo -e "  ${GREEN}Passed: ${PASS}${NC} / ${TOTAL}"
if [ "$FAIL" -gt 0 ]; then
    echo -e "  ${RED}Failed: ${FAIL}${NC} / ${TOTAL}"
    exit 1
else
    echo -e "  ${GREEN}All tests passed!${NC}"
    exit 0
fi
