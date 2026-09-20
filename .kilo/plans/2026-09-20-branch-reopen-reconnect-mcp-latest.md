# Branch Rebind, Reconnect Exposure, and MCP 2026-07-28 — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop tokensave MCP servers from serving the wrong git branch across harnesses, give every harness an in-session "Reconnect" that does not depend on host UI, and bring the local MCP server up to the current MCP specification revision (2026-07-28).

**Architecture:** The wrong-branch symptom has one mechanism with two on-ramps: every server process resolves the branch DB once at startup (`TokenSave::open` → `resolve_db_for_branch`) and holds it for life, so an untracked branch silently falls back to an ancestor DB and a mid-session `git checkout` trips the #400 drift refusal. The fix adds an in-session reopen tool that re-resolves the branch and swaps the graph handle, makes the post-checkout hook prevent the untracked case by default on install, teaches every refusal/warning message to name the in-session recovery path, and upgrades protocol negotiation to the 2026-07-28 spec.

**Tech Stack:** Rust 2021, clap, tokio, serde_json, libsql. MCP JSON-RPC 2.0 over stdio. Tests: `cargo test` (integration tests in `tests/`, in-module `#[cfg(test)]`).

## Global Constraints

- Upstream-default compatibility: never change behavior for a working install. New behavior ships off by default or behind install-time opt-in, matching the repo's #372/#397 precedent.
- The #400 corruption guard is inviolable: no code path may write one branch's working-tree files into another branch's DB. Reopen swaps the DB handle first, then syncs.
- Every new MCP tool must be registered in `src/mcp/tools/definitions.rs` AND in the tool-count/drift-classification tests (precedent: commit 6c55e24).
- Hook and MCP-server code must stay silent on success (a hook runs on every checkout; a server narrates on every call).
- Conventional Commits for every commit: `type(scope): imperative summary`.
- Working branch: `feat/branch-reopen-and-mcp-latest` (from master @ 77e0c0e).

## Evidence Base (verified 2026-09-20)

- Startup-only branch binding: `src/tokensave/mod.rs:215-344` (`open`), `:358-427` (`resolve_db_for_branch` untracked→ancestor fallback + warning).
- Per-call drift detection + refusal: `src/tokensave/staleness.rs:205-262` (`branch_drift`), `src/mcp/server.rs:780-835` (`strict_tree_refusal`, `branch_drift_refusal` — message says "Restart the MCP server"), `:2616-2624` (drift WARNING banner).
- No in-session reopen exists: `src/mcp/graph_scope.rs:296-350` rejects selecting another branch of the served project. A dr-house-verifier pass (2026-09-20) searched for any reopen/swap path and found none.
- `auto_track` defaults false: `src/config.rs:82-110`; hook gate: `src/commands.rs:1036-1104` + `src/agents/hooks.rs:381-472`.
- MCP protocol: `src/mcp/server.rs:455-496` — `SUPPORTED_PROTOCOL_VERSIONS` ends at `2025-11-25`; current spec is `2026-07-28` (stateless core, `initialize`/`initialized` removed, `server/discover` added, per-request `_meta` carries `io.modelcontextprotocol/protocolVersion`; error `-32022` `UnsupportedProtocolVersionError`; `notifications/message` deprecated). Sources: modelcontextprotocol.io spec/versioning pages, 2026-07-28 release post.
- Harness reconnect landscape (external, cited): Claude Code CLI has `/mcp reconnect <name>` (since v1.0.64; re-reads config) but stdio servers are never auto-reconnected (anthropics/claude-code#43177); Claude Desktop has no reconnect affordance for stdio at all (#54136, #59274). Therefore the only fix that covers every harness is server-side self-service reopen.
- Local install state: binary `~/.local/bin/tokensave` = 7.12.1 = latest release; Claude and Codex configs point at the `tokensave-otel` zsh wrapper → `tokensave-telemetry-relay.py proxy` → `~/.local/bin/tokensave serve --timings` (same current binary); Cursor points directly at the binary. Binary freshness is fine; protocol freshness is not.

---

### Task 1: `tokensave_reopen` — in-session branch rebind ("Reconnect" core)

**Files:**
- Modify: `src/mcp/server.rs` (add `reopen_graph`, swap `cg`; exempt tool from drift/strict refusals is automatic — see Interfaces)
- Modify: `src/mcp/tools/definitions.rs` (register `tokensave_reopen`)
- Modify: `src/mcp/tools/handlers/mod.rs` (dispatch entry)
- Create: `tests/reopen_test.rs`
- Modify: `tests/mcp_test.rs` (tool-count expectation 87/88 → 88/89)

**Interfaces:**
- Consumes: `TokenSave::open(project_root) -> Result<Self>` (`src/tokensave/mod.rs:215`); `McpServer::refresh_file_token_map` (used by `run_version_reindex`); servers registry write path (`src/servers.rs`, re-register replaces the row for a PID — see `re_registering_replaces_rather_than_duplicates` test).
- Produces: `McpServer::reopen(&self) -> Result<String>` — re-opens the same `project_root`, swaps the live graph, refreshes serving state, returns a one-line human summary naming the old and new serving branch. Registered MCP tool name: `tokensave_reopen` (Kilo-namespace: `tokensave_tokensave_reopen`).

Design decisions (from the Foreman diagnosis + verifier pass):
1. **Swap shape:** move `cg: TokenSave` behind `std::sync::RwLock<TokenSave>`. Readers take `read()`; `reopen` takes `write()`. This is the minimal change — `graph_scope` already builds per-call dispatch values, and `handle_tools_call` receives the graph as a value today, so the swap point is one lock acquisition per call, not a refactor of every handler.
2. **Sync safety:** refuse reopen while a lazy sync is in flight (poll `lazy_sync_in_flight: AtomicBool`; if set, return "a sync is in flight; retry in a moment" — do not block, do not cancel; #396/#450 precedent). After swapping, run one `run_startup_catch_up_sync`-equivalent against the new DB so the fresh handle is current.
3. **State refresh:** after swap, recompute `worktree_mismatch`, `serving_branch`/fallback fields (they live on the new `TokenSave`), `file_token_map`, and rewrite the `~/.tokensave/servers/<pid>.json` entry's `db_path`.
4. **Session state:** reset `SessionState::unscanned_shown` for the served root (a new DB means the "already shown" flags describe a different graph). Leave `tool_call_counts` and stats alone — they describe the server process, not the graph.
5. **Refusal exemption is free:** `branch_drift_refusal` (`server.rs:821-827`) only refuses tools in `graph_scoped_tools`/`selectorless_local_graph_tools`; `tokensave_reopen` is in neither set, so it stays callable during drift — exactly like `tokensave_status`. Add a test pinning this.
6. **Untracked-branch reopen:** reopen re-runs `open`, which applies the same untracked→ancestor fallback with warning. Do NOT auto-`branch add` inside reopen (write path, gated by `auto_track` — #397 keeps that knob authoritative). The tool's response text includes the fallback warning when one applies.

- [ ] **Step 1: Write the failing test** — `tests/reopen_test.rs`

```rust
//! tokensave_reopen: a live server must rebind to the working tree's branch (#400 follow-up).
#![cfg(feature = "test-transport")]

use serde_json::{json, Value};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;
use tempfile::TempDir;
use tokensave::mcp::transport::ChannelTransport;
use tokensave::mcp::McpServer;
use tokensave::tokensave::TokenSave;

fn git(root: &Path, args: &[&str]) {
    let out = Command::new("git").args(args).current_dir(root)
        .env("GIT_AUTHOR_NAME", "TokenSave Test")
        .env("GIT_AUTHOR_EMAIL", "tokensave@example.com")
        .env("GIT_COMMITTER_NAME", "TokenSave Test")
        .env("GIT_COMMITTER_EMAIL", "tokensave@example.com")
        .output().expect("run git");
    assert!(out.status.success(), "git {} failed", args.join(" "));
}

async fn call(server: &Arc<McpServer>, name: &str, arguments: Value) -> Value {
    let (mut transport, _s, mut receiver) = ChannelTransport::new();
    let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":name,"arguments":arguments}}).to_string();
    server.handle_and_write(&request, &mut transport).await;
    let raw = receiver.recv().await.expect("expected a response");
    serde_json::from_str(raw.trim()).expect("valid JSON-RPC")
}

async fn drifted_server() -> (TempDir, Arc<McpServer>) {
    let dir = TempDir::new().unwrap();
    let root = dir.path().to_path_buf();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/base.rs"), "fn base() -> i32 { 1 }\n").unwrap();
    git(&root, &["init", "-b", "master"]);
    git(&root, &["add", "-A"]); git(&root, &["commit", "-m", "base"]);
    let cg = TokenSave::init(&root).await.unwrap();
    cg.index_all().await.unwrap(); drop(cg);
    git(&root, &["checkout", "-b", "feature"]);
    tokensave::branch::track_branch_copy(&root, &root.join(".tokensave"), "feature").await.unwrap();
    git(&root, &["checkout", "master"]);
    git(&root, &["checkout", "feature"]);
    std::fs::write(root.join("src/feature_only.rs"), "fn feature_only() -> i32 { 2 }\n").unwrap();
    let cg = TokenSave::open(&root).await.unwrap(); // opened on master; tree is on feature
    let server = McpServer::new_explicit_root(cg, None).await;
    (dir, server)
}

#[tokio::test]
async fn reopen_rebinds_a_drifted_server_to_the_working_branch() {
    let (_dir, server) = drifted_server().await;
    // Precondition: drift refusal fires for a local graph tool.
    let refused = call(&server, "tokensave_search", json!({"query": "feature_only"})).await;
    assert_eq!(refused["error"]["code"], -32600, "precondition: drift refusal");

    // The tool itself must be callable during drift.
    let reopened = call(&server, "tokensave_reopen", json!({})).await;
    assert!(reopened.get("error").is_none(),
        "tokensave_reopen must stay callable during drift, got {reopened:?}");

    // After reopen: no refusal, and the feature-only symbol is found.
    let after = call(&server, "tokensave_search", json!({"query": "feature_only"})).await;
    assert!(after.get("error").is_none(),
        "reopen must clear the drift condition, got {after:?}");
    let text = serde_json::to_string(&after).unwrap();
    assert!(text.contains("feature_only"), "post-reopen search must find the feature-only symbol");
}

#[tokio::test]
async fn reopen_is_idempotent_when_not_drifted() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    git(root, &["init", "-b", "master"]);
    git(root, &["add", "-A"]); git(root, &["commit", "-m", "base"]);
    let cg = TokenSave::init(root).await.unwrap(); cg.index_all().await.unwrap(); drop(cg);
    let cg = TokenSave::open(root).await.unwrap();
    let server = McpServer::new_explicit_root(cg, None).await;
    for _ in 0..2 {
        let r = call(&server, "tokensave_reopen", json!({})).await;
        assert!(r.get("error").is_none(), "reopen on a matched tree must succeed: {r:?}");
    }
    let search = call(&server, "tokensave_search", json!({"query": "main"})).await;
    assert!(search.get("error").is_none(), "graph must keep answering after reopen");
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo test --features test-transport --test reopen_test` → FAIL: no method `reopen`, unknown tool `tokensave_reopen`.

- [ ] **Step 3: Implement the swap.** In `src/mcp/server.rs`:
  - Change field `cg: TokenSave` → `cg: std::sync::RwLock<TokenSave>`.
  - Add a private accessor `fn with_cg<T>(&self, f: impl FnOnce(&TokenSave) -> T) -> T { f(self.cg.read().unwrap()) }` and update the read sites (`handle_tools_call`, `branch_drift`, `server_stats_json`, resource readers) to use it — mechanical, one lock per call, matching the existing per-call dispatch style.
  - Add:

```rust
/// Re-resolves the project's branch DB and swaps it in — the in-session
/// "reconnect". Called by the `tokensave_reopen` tool; safe under drift by
/// construction (the swap happens before any sync, so writes land in the
/// new branch's DB — the #400 invariant).
pub async fn reopen(&self) -> crate::errors::Result<String> {
    if self.lazy_sync_in_flight.load(std::sync::atomic::Ordering::Acquire) {
        return Err(crate::errors::TokenSaveError::Config {
            message: "a sync is in flight; retry tokensave_reopen in a moment".into(),
        });
    }
    let project_root = self.with_cg(|cg| cg.project_root().to_path_buf());
    let fresh = TokenSave::open(&project_root).await?;
    let new_branch = fresh.serving_branch_or_active().map(str::to_string);
    let old_branch = self.with_cg(|cg| cg.serving_branch_or_active().map(str::to_string));
    {
        let mut guard = self.cg.write().unwrap();
        *guard = fresh;
    }
    self.refresh_file_token_map().await;
    self.reset_session_state_for_served_root();
    self.reregister_server_entry_db_path().await; // servers/<pid>.json db_path
    self.run_startup_catch_up_sync().await;      // bounded by the same guards as startup
    Ok(format!(
        "reopened: serving '{}' (was '{}')",
        new_branch.as_deref().unwrap_or("<none>"),
        old_branch.as_deref().unwrap_or("<none>"),
    ))
}
```

  (Add `serving_branch_or_active` on `TokenSave` returning `self.serving_branch.as_deref().or(self.active_branch.as_deref())` — both fields already exist, `src/tokensave/mod.rs:49-56`.)
  - Register the tool in `src/mcp/tools/definitions.rs` with an empty-arg schema:

```rust
def("tokensave_reopen", "Reopen Graph",
    "Re-resolve this project's git branch and rebind this server's index to it, \
     in-session — no host restart needed. Use after a `git checkout` or worktree \
     switch, or whenever a tool response warns about branch drift or serving from \
     another branch. Single call; returns the newly served branch.",
    json!({"type": "object", "properties": {}})),
```

  - Dispatch in `src/mcp/tools/handlers/mod.rs`: `"tokensave_reopen" => info::handle_reopen(cg, /* server handle via session */)`. NOTE: the handler signature takes `&TokenSave`; the reopen path needs the server. Route it as a server-level method instead: in `McpServer::handle_request`, intercept `"tokensave_reopen"` **before** the generic `tools/call` dispatch the same way `tokensave_status` gets `server_stats` injected — call `self.reopen().await` and format the result as a tool response. This avoids threading an `Arc<McpServer>` into handlers.
- [ ] **Step 4: Update tool-count tests** — `tests/mcp_test.rs` expected counts 87/88 → 88/89 (the `ast_grep_available()` conditional in that test is the precedent to follow). Run the drift-classification/tool-count suite the 6c55e24 commit touched.
- [ ] **Step 5: Run** — `cargo test --features test-transport --test reopen_test --test mcp_test --test branch_drift_test --test strict_tree_mismatch_test` → PASS all.
- [ ] **Step 6: Commit** — `git add -A && git commit -m "feat(mcp): tokensave_reopen rebinds a live server to the working branch"`

### Task 2: Messages and instructions expose the reconnect path in every harness

**Files:**
- Modify: `src/mcp/server.rs:804-807, 831-834, 2617-2623` (drift/strict refusal + WARNING text)
- Modify: `src/tokensave/mod.rs` fallback warning (`fallback_warning` construction near `resolve_db_for_branch`)
- Modify: `src/mcp/server.rs:1838-1865` (`BASE_INSTRUCTIONS` in `handle_initialize`)
- Test: extend `tests/branch_drift_test.rs`, `tests/strict_tree_mismatch_test.rs` assertions

**Interfaces:**
- Consumes: Task 1's `tokensave_reopen`.
- Produces: user/agent-visible strings; no API change.

The current messages tell the agent to do something the session cannot do from inside ("Restart the MCP server", "reopen it for the working branch" — a capability that does not exist; `graph_scope.rs:324-327` rejects branch reselection). After Task 1 both statements become actionable.

- [ ] **Step 1: Update the three drift/strict/refusal strings** to end with: `"...Call tokensave_reopen to rebind this server in-session, or restart the MCP server if that fails."` Keep naming both branches (existing tests assert that).
- [ ] **Step 2: Append to the untracked-branch fallback warning** (`branch 'X' is not tracked — serving from 'Y'`): `"...Call tokensave_reopen after tracking it, or run 'tokensave branch add X'."`
- [ ] **Step 3: Add one sentence to `BASE_INSTRUCTIONS`** returned at `initialize` so every harness's model learns the recovery path without reading docs: `"After a git checkout, worktree switch, or any branch-mismatch warning from these tools, call tokensave_reopen once to rebind; do not restart the session."`
- [ ] **Step 4: Extend the existing message-assertions** in `tests/branch_drift_test.rs` (drift refusal test asserts `contains("Restart")` today) to also assert `contains("tokensave_reopen")`. Run: `cargo test --features test-transport --test branch_drift_test --test strict_tree_mismatch_test` → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(mcp): drift messages and server instructions name tokensave_reopen"`

### Task 3: Prevent the untracked-branch fallback — auto-track on install

**Files:**
- Modify: `src/agents/integrations/claude.rs` (+ each `install_mcp_server` writer that supports `env`: kimi, omp, qwen, cursor, plank, gemini, codex, opencode as applicable)
- Modify: `src/agents/mod.rs` or a shared helper — add `const MCP_SERVER_ENV: serde_json::Value = json!({"TOKENSAVE_AUTO_TRACK": "1"})` and merge it into the server entry on install; preserve any user env keys already present (merge, do not replace).
- Test: `tests/agent_test.rs` (extend the install-assertion helpers to check the `env` key)

**Interfaces:**
- Consumes: existing `env_bool_override("TOKENSAVE_AUTO_TRACK", config.auto_track)` (`src/tokensave/mod.rs:225-227`, `src/commands.rs:63-69`) — the env var is already authoritative everywhere the hook and `open` run.
- Produces: MCP server entries shaped `{command, args: ["serve"], env: {"TOKENSAVE_AUTO_TRACK": "1"}}`.

Rationale: upstream keeps `auto_track` default-off (#397, opt-in philosophy), so we do NOT flip the config default. Opt-in moves to install time, where it composes with `preserve_mcp_command` (issue #161 keeps custom commands; env merge follows the same preserve-user-choices rule). Codex's TOML writer gets `env = { "TOKENSAVE_AUTO_TRACK" = "1" }` under `[mcp_servers.tokensave]`.

- [ ] **Step 1: Write the failing test** in `tests/agent_test.rs`, modeled on `test_claude_install_creates_config`: assert `content["mcpServers"]["tokensave"]["env"]["TOKENSAVE_AUTO_TRACK"] == "1"` after install, and that a pre-seeded user env key survives a reinstall.
- [ ] **Step 2: Run** — `cargo test --test agent_test` → FAIL on the new assertions.
- [ ] **Step 3: Implement the env merge** in each JSON writer: build the entry with `"env"` merged from the previous entry's `env` object (if any) plus the constant. TOML writer (codex) does the same with `toml::Value::Table`.
- [ ] **Step 4: Run** — `cargo test --test agent_test` → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(agents): install MCP entries with TOKENSAVE_AUTO_TRACK=1"`

### Task 4: MCP 2026-07-28 — current spec revision for local servers

**Files:**
- Modify: `src/mcp/server.rs:455-496` (version tables), `:1798+` (`handle_request` method match), `:1824-1880` (`handle_initialize`)
- Modify: `src/mcp/transport.rs` (request `_meta` access helper)
- Test: extend `tests/serve_disable_test.rs` / new `tests/mcp_protocol_2026_test.rs`

**Interfaces:**
- Consumes: existing `negotiate_protocol_version(params)` + `SUPPORTED_PROTOCOL_VERSIONS`.
- Produces: a server that (a) answers `2026-07-28` when a client requests it, (b) implements `server/discover`, (c) tolerates requests with no prior `initialize` (the 2026-07-28 handshake is gone), (d) reads per-request `params._meta["io.modelcontextprotocol/protocolVersion"]` and returns `-32022` (`UnsupportedProtocolVersionError`, supported versions in `data`) when it names an unsupported one.

Scope guard (read the spec pages before coding: modelcontextprotocol.io/docs/2026-07-28/learn/versioning, /server/discover, changelog SEP-2575/2596): for stdio servers the 2026-07-28 duties that matter here are version-per-request, `server/discover`, and no-required-handshake. We do NOT implement Tasks/MRTR/MCP Apps/extensions — they are optional extensions. `notifications/message` stays (deprecated ≠ removed; older clients still use it).

- [ ] **Step 1: Failing test** — `tests/mcp_protocol_2026_test.rs`: (1) `initialize` with `protocolVersion: "2026-07-28"` echoes `"2026-07-28"` in the response; (2) a `tools/call` carrying `_meta` version `2026-07-28` with NO prior `initialize` still answers (stateless entry); (3) `server/discover` returns `{"protocolVersions": [...all four...], "serverInfo": {...}}`; (4) a request naming `_meta` version `"1999-01-01"` gets error code `-32022`.
- [ ] **Step 2: Run** — `cargo test --features test-transport --test mcp_protocol_2026_test` → FAIL.
- [ ] **Step 3: Implement.**
  - `const SUPPORTED_PROTOCOL_VERSIONS: [&str; 5] = ["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];`
  - In `handle_request`: add `"server/discover" => Some(self.handle_discover(id))`; build the response from the version table + `serverInfo` (same shape as `handle_initialize` emits).
  - Add `_meta` version check at the top of `handle_request` for non-notification requests: if `params._meta["io.modelcontextprotocol/protocolVersion"]` is present and not in the table, return `{"code": -32022, "message": "UnsupportedProtocolVersionError", "data": {"supported": [...]}}`. Absent `_meta` keeps legacy behavior (old clients are the majority today).
  - Remove the hard dependency on a prior `initialize`: today the peeked-initialize path exists; make the server accept `tools/call`/`resources/*` without it (track `initialized: AtomicBool` for legacy logging only).
- [ ] **Step 4: Run full protocol surface** — `cargo test --features test-transport --test mcp_protocol_2026_test --test serve_disable_test --test mcp_server_test` → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(mcp): support protocol revision 2026-07-28 (discover, per-request version, handshakeless stdio)"`

### Task 5: Doctor surfaces reconnect + protocol freshness

**Files:**
- Modify: `src/agents/integrations/claude.rs` healthcheck (+ shared doctor helpers) — report `tokensave_reopen` availability and the server's negotiated protocol revision
- Modify: `src/mcp/tools/handlers/info.rs` (`handle_status`) — add `"served_protocol_versions"` to `tokensave_status` output
- Test: extend `tests/agent_test.rs` healthcheck tests

- [ ] **Step 1:** Add to `tokensave_status` output: `"recovery": "tokensave_reopen available in-session"` and the supported-protocol list (cheap: both are static).
- [ ] **Step 2:** Doctor: after the MCP-registered check, verify the registered `command` binary version matches the running binary (the staleness check exists; extend it to print both versions on mismatch) — this is the "are local MCPs on the latest version" check, runnable on demand: `tokensave doctor --agent claude`.
- [ ] **Step 3:** Run `cargo test --test agent_test` and `cargo test --features test-transport --test mcp_handler_test` → PASS.
- [ ] **Step 4:** Commit — `git commit -m "feat(doctor): report reopen availability and MCP protocol freshness"`

### Task 6: Documentation + release notes

**Files:**
- Modify: `docs/BRANCHING-USER-GUIDE.md:110-138` — replace "You do need to restart the MCP server after a `git checkout`" guidance with the reopen flow (restart stays as fallback).
- Modify: `CHANGELOG.md` — one line per user-visible change, matching existing entry style.

- [ ] **Step 1:** Update both docs with the exact tool name and the per-harness reality table: Claude Code also has `/mcp reconnect tokensave` (stdio never auto-reconnects, anthropics/claude-code#43177); Claude Desktop has no reconnect UI (→ reopen tool is the only path); Cursor/Codex/others: restart the harness or rely on the in-session tool.
- [ ] **Step 2:** Commit — `git commit -m "docs: reconnect guidance for branch drift across harnesses"`

## Final verification (whole plan)

```bash
cargo test --features test-transport
cargo test
cargo clippy --all-targets -- -D warnings
```

Plus a live smoke on this machine (local harnesses, no harness restart needed): from a tokensave-tracked project, `git checkout` to a different branch and call `tokensave_tokensave_reopen` through the running session's MCP server; confirm `tokensave_status` reports the new serving branch.

## Out of scope / external

- Claude Desktop's missing reconnect UI is upstream (anthropics/claude-code#54136); nothing in this repo can add it. The reopen tool removes the dependency on it.
- Flipping upstream `auto_track` default: deliberately not done (#397 opt-in philosophy); install-time env achieves the machine-wide effect reversibly.
- MCP Tasks/MRTR/MCP Apps extensions: optional in 2026-07-28, not needed by any current local harness.

## Open questions resolved during execution (record answers in the plan when found)

1. `handle_tools_call` signature: reopen is intercepted at server level (Task 1 Step 3 note) — confirm no double-dispatch.
2. Whether `run_startup_catch_up_sync` after reopen must reset `last_staleness_check_at` (it should, same as startup).
3. Codex TOML writer's env merge: confirm `toml` crate round-trips inline tables acceptably in `~/.codex/config.toml`.
