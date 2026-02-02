# Agent Loop: OpenClaw Parity & Rust-Native Improvements

Date: 2026-02-01

## Current State

Moltis already implements the core agent loop and most foundational features:

- Core agentic loop: LLM -> tool calls -> repeat (`crates/agents/src/runner.rs`)
- Trait-based `LlmProvider` with 7 providers (Anthropic, OpenAI, GitHub Copilot, Kimi Code, Codex, GenAI, AsyncOpenAI)
- Trait-based `AgentTool` + `ToolRegistry` (`crates/agents/src/tool_registry.rs`)
- Concurrent tool execution via `futures::join_all`
- 6-layer tool policy with allow/deny globs (`crates/tools/src/policy.rs`)
- Approval system for dangerous commands (`crates/tools/src/approval.rs`)
- Streaming via SSE/WebSocket with `StreamEvent` enum
- Session persistence: JSONL messages + SQLite metadata
- Auto-compaction at 95% context window (`crates/gateway/src/chat.rs:386-462`)
- Sandbox execution: Docker + Apple Container backends (`crates/tools/src/sandbox.rs`)
- Skills system with YAML frontmatter discovery (`crates/skills/src/`)
- Event broadcasting: Thinking, ToolCallStart/End, TextDelta, Iteration
- Abort/cancellation via tokio `AbortHandle`

---

## Feature Gaps (OpenClaw has, Moltis doesn't)

### 1. Session Run Serialization (High Priority)

OpenClaw serializes runs per-session using queues to prevent race conditions
and history corruption when multiple messages arrive concurrently.

Moltis spawns tokio tasks with no per-session ordering guarantee.

**Plan:** Add a per-session `tokio::sync::Mutex` or MPSC channel in
`chat.rs` that ensures only one agent run executes per session at a time.
Incoming messages while a run is active are handled by the message queue
mode (see #6).

### 2. Hooks: Wire Remaining Events (High Priority) — DONE

**Status: Complete.** The hook system is now comprehensive:
- 15 `HookEvent` variants defined with typed `HookPayload` structs
- `HookRegistry` with priority ordering, circuit breaker (3 failures → disable), async/sync dispatch
- `HookHandler` trait with `handle()` / `handle_sync()` + `HookAction` (Continue/ModifyPayload/Block)
- Read-only vs modifying event classification (parallel vs sequential dispatch)
- Shell hooks via `ShellHookHandler` (stdin JSON, exit code protocol)
- WASM hooks stubbed (`wasm_hook.rs`)
- Hook discovery from `.moltis/hooks/` (project) and `~/.moltis/hooks/` (user) with `HOOK.md` frontmatter
- Eligibility checks (OS, binaries on PATH, env vars)
- 3 bundled hooks: `boot-md`, `session-memory`, `command-logger`
- 6 example shell hooks in `examples/hooks/`
- CLI: `moltis hooks list`, `moltis hooks info <name>`

**All 15/15 events now fired:**

| Event | Location | Behavior |
|-------|----------|----------|
| BeforeAgentStart | `runner.rs` loop entry | Can block agent start |
| AgentEnd | `runner.rs` loop exit | Read-only (metrics, logging) |
| MessageReceived | `chat.rs` send() | Read-only (audit trail) |
| MessageSending | `runner.rs` before LLM call | Can block (content filtering) |
| MessageSent | `runner.rs` after LLM response | Read-only (audit trail) |
| ToolResultPersist | `runner.rs` before appending result | Can modify (redaction) |
| SessionStart | `session.rs` resolve() on new session | Read-only |
| SessionEnd | `session.rs` delete() | Read-only |
| BeforeToolCall | `runner.rs` | Can block/modify args |
| AfterToolCall | `runner.rs` | Read-only |
| BeforeCompaction | `chat.rs` compact() | Can block |
| AfterCompaction | `chat.rs` compact() | Read-only |
| GatewayStart | `server.rs` | Read-only |
| GatewayStop | `server.rs` | Read-only |
| Command | bundled hooks | Read-only |

### 3. Agent-Level Timeout Enforcement (High Priority)

OpenClaw enforces configurable agent-level timeouts (default 600s). Moltis
has tool-level timeouts but no overall run timeout.

**Plan:** Wrap the agent loop future in `tokio::time::timeout(duration, ...)`.
Make the duration configurable per-agent in config. On timeout, return a
structured error and emit a lifecycle event.

### 4. Retry After Compaction (High Priority)

OpenClaw retries the LLM call after auto-compaction triggers (resets
buffers, avoids tool summary duplication). Moltis compacts but doesn't
retry the failed call.

**Plan:** In `run_agent_loop`, catch context-window-exceeded errors from
the provider. When detected: trigger compaction on the session history,
rebuild the message list from compacted history, retry the current
iteration. Limit to 1 retry per iteration to avoid infinite loops.

### 5. Sub-Agent / Nested Agent Support (Medium Priority)

OpenClaw can spawn sub-agents (the Task tool pattern — a tool that runs
another agent loop). Moltis has no recursive agent invocation.

**Plan:** Create a `SpawnAgentTool` that calls `run_agent_loop` with its
own system prompt, tool subset, and iteration limit. The sub-agent result
is returned as the tool output. Needs careful stack management (limit
nesting depth, propagate cancellation).

### 6. Message Queue Modes (Medium Priority)

OpenClaw supports three modes for messages arriving during an active run:
- **collect**: buffer and process after current run completes
- **steer**: inject into the current run's context
- **followup**: queue as a new run after current completes

Moltis has no such mechanism.

**Plan:** Add a `MessageQueueMode` enum and per-session message buffer.
When `chat.send()` is called during an active run, apply the configured
mode. Default to `followup`.

### 7. Tool Result Sanitization (Medium Priority)

OpenClaw sanitizes tool results before logging and feeding back to LLM:
size limits, base64 image stripping, content truncation.

Moltis passes raw `serde_json::Value` results without limits.

**Plan:** Add `sanitize_tool_result(value: Value, max_bytes: usize) -> Value`
in `runner.rs`. Truncate strings exceeding the limit, strip base64-encoded
image data, and add a `[truncated]` marker. Apply before appending to
message history.

### 8. Run Lifecycle Phases (Medium Priority)

OpenClaw emits structured lifecycle events: `queued -> running ->
tool_executing -> completing -> done/error`. Moltis has ad-hoc events
without a clear state machine.

**Plan:** Add a `RunPhase` enum and emit `RunnerEvent::PhaseChange(RunPhase)`
at each transition. This gives consumers (UI, logging, metrics) a single
event to track run progress.

### 9. `NO_REPLY` Filtering & Reply Assembly (Low Priority)

OpenClaw filters special tokens like `NO_REPLY` from final output and
assembles replies from text + optional reasoning + inline tool summaries.

**Plan:** Post-process the final response in `runner.rs` to strip control
tokens. Add an option to include inline tool call summaries in the final
text (useful for non-streaming consumers).

### 10. Plugin Loading Architecture (Low Priority)

OpenClaw has a full plugin system with loadable plugins that hook into
gateway and agent lifecycle. Moltis has the hooks enum but no plugin
loader.

**Plan:** Define a `Plugin` trait with methods for each `HookEvent`.
Add a plugin registry loaded at gateway startup from a config-specified
directory. Plugins are dynamic libraries (`.so`/`.dylib`) or WASM modules.

---

## Rust-Native Feature Ideas

### A. `tower::Service`-Based Tool Middleware

Model each tool as a `tower::Service` and use `tower::Layer` for
cross-cutting concerns. This composes naturally:

```rust
let tool = ApprovalLayer::new(policy)
    .layer(TimeoutLayer::new(Duration::from_secs(30)))
    .layer(MetricsLayer::new())
    .layer(raw_tool);
```

Benefits: rate limiting, retries, metrics, and approval checks become
reusable layers instead of ad-hoc logic scattered through the runner.

### B. `tokio::JoinSet` for Tool Execution

Replace `join_all` with `JoinSet` for concurrent tool execution. Gives
cancellation propagation — if the run is aborted, all in-flight tools
are cancelled automatically. Also allows processing results as they
complete (first-finished ordering).

### C. Trait-Based Compaction Strategies

```rust
trait CompactionStrategy: Send + Sync {
    async fn compact(
        &self,
        history: &[Message],
        provider: &dyn LlmProvider,
    ) -> anyhow::Result<Vec<Message>>;
}
```

Implementations:
- `SummarizationCompaction` — current behavior (LLM summarizes)
- `SlidingWindowCompaction` — drop oldest messages, keep recent N
- `ImportanceRankedCompaction` — use embeddings to keep high-value messages

### D. Cost Tracking Trait

```rust
trait CostTracker: Send + Sync {
    fn record_usage(&self, provider: &str, model: &str, usage: &Usage);
    fn budget_remaining(&self) -> Option<f64>;
}
```

Enforce per-session or per-user token/cost budgets. Automatically stop
runs that exceed limits. Expose via API for UI display.

### E. Deterministic Replay / Audit Log

Log every LLM request+response and tool call+result as a structured
event stream (append-only file or database). Replay a session
deterministically for debugging by feeding recorded responses instead of
calling the LLM. Useful for:
- Debugging production issues
- Regression testing agent behavior
- Cost analysis (replay without API calls)

### F. Backpressure-Aware Event Streaming

Replace callback-based event emission with `tokio::sync::broadcast`
channels. Consumers (WebSocket, logging, metrics) subscribe independently.
Slow consumers get `Lagged` errors instead of blocking the agent loop.

### G. Typed Tool Results

Instead of `serde_json::Value` everywhere, support `ToolResult<T: Serialize>`
for compile-time guarantees on tool output shapes. The trait can have an
associated type:

```rust
trait AgentTool: Send + Sync {
    type Output: Serialize;
    async fn execute(&self, params: Value) -> Result<Self::Output>;
}
```

Use type erasure at the registry boundary to maintain dynamic dispatch.

---

## Suggested Priority Order

### Phase 1 — Core Correctness
1. Session run serialization
2. ~~Wire remaining 7 hook events~~ ✓ Done
3. Agent-level timeout
4. Retry after compaction

### Phase 2 — Feature Parity
5. Sub-agent tool
6. Message queue modes
7. Result sanitization
8. Run lifecycle phases

### Phase 3 — Rust Advantages
9. `JoinSet` for cancellation propagation
10. `tower::Service` tool middleware
11. Compaction strategy trait
12. Backpressure-aware event streaming
13. Cost tracking
14. Deterministic replay
