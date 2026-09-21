# Branch Attachment and Cross-Host Reconnect Implementation Plan

> **For agentic workers:** Execute this plan task by task with fresh implementation and review agents. Track progress with the checkboxes. Do not invoke `using-superpowers`, commit, push, create a pull request, or alter remotes unless Bobby explicitly authorizes that action.

**Goal:** Make every local TokenSave MCP server follow Git branch changes without a host restart, expose an in-session **Reconnect TokenSave Graph** tool in Claude and other MCP hosts, prevent untracked branches from silently reading or updating another branch's database, and make local MCP configuration and live-process freshness measurable.

**Architecture:** TokenSave will model the live relationship among the working branch, the served branch, and the database selected for that branch. The server will own a swappable graph handle guarded by an asynchronous read-write lock. Tracked branch changes will rebind automatically before a local graph call. The always-loaded `tokensave_reopen` tool will provide explicit recovery and may track an untracked branch because the tool advertises `readOnlyHint: false`. Installers will start MCP servers with `serve --auto-track`, and an integration-config revision will refresh every tracked host even when the package version is unchanged.

**Tech Stack:** Rust 2021, Tokio, clap, serde, serde_json, libsql, sha2, MCP JSON-RPC 2.0 over stdio. Integration and transport tests live under `tests/`.

## Global Constraints

- Preserve the #400 invariant: no automatic or explicit sync may write one branch's working-tree files into another branch's database.
- Preserve current single-database behavior. A project with only the default database continues syncing across untracked branch names.
- Treat **graph rebind**, **process restart**, **configuration reload**, **tool-schema refresh**, and **protocol negotiation** as different operations.
- `tokensave_reopen` performs graph rebind only. It does not claim to reload a binary, reread host configuration, or renegotiate MCP.
- Keep MCP 2026-07-28 implementation out of this plan. Use the companion plan at `.kilo/plans/2026-09-21-mcp-2026-07-28-modernization.md`.
- Keep successful hooks and automatic branch rebinds silent.
- Every new tool must appear in `src/mcp/tools/definitions.rs`, tool-count tests, write-tool annotation tests, permissions, and drift classification.
- Run Git-dependent tests with isolated XDG and Git configuration. This machine's global ignore contains `/.tokensave/` and otherwise changes test behavior.
- Working branch: `feat/branch-reopen-and-mcp-latest`, based on `master` at `77e0c0e`.
- Current delivery state: `origin` points to `aovestdipaperino/tokensave`; no `bobbypierce42/tokensave` fork exists; the feature branch has no upstream. Do not push until Bobby authorizes the fork, remote changes, and push target.

## Verified Diagnosis

### Ranked cause set

1. **Proximate cause:** `TokenSave::open` resolves the branch database once, and `McpServer` stores the resulting `TokenSave` by value for the process lifetime (`src/tokensave/mod.rs:215-427`, `src/mcp/server.rs:331-447`). Git HEAD is reread by `branch_drift`, but every current response path refuses or warns instead of replacing the served handle (`src/tokensave/staleness.rs:198-236`, `src/mcp/server.rs:780-835,2610-2631`). Two independent diagnostic workers and a `dr-house-verifier` confirmed this mechanism. Seven focused drift tests passed.
2. **Root cause:** stdio hosts own child-process lifetime while TokenSave owns branch/database attachment. The code provides no in-band seam between those responsibilities. The running-server registry intentionally identifies processes without stopping them (`src/servers.rs:1-48`).
3. **Root cause:** the only current remedy is a host restart. Claude Desktop does not expose a dependable stdio reconnect action, and other MCP hosts differ. The server instructions contain no recovery tool (`src/mcp/server.rs:1838-1845`).
4. **Contributing factor:** in multi-database mode, checkout to an untracked branch returns `None` from `branch_drift`. Content tools, `tokensave_status`, and startup-cached fallback fields emit no new signal (`src/tokensave/staleness.rs:220-231`, `src/mcp/tools/handlers/info.rs:33-50`). A verifier confirmed this behavior and reran `branch_drift_test` successfully.
5. **Contributing factor:** installation refresh has specific gaps. Patch-only version changes skip reinstall, no integration writer opts into automatic tracking, existing wrapper commands are preserved without version validation, only `installed_agents` are refreshed, and a config rewrite cannot alter a running process (`src/agents/mod.rs:24-94,234-287`, `src/main.rs:223-299`). A verifier confirmed all five statements.

### Evidence limitations

- Existing tests prove the handle remains frozen and the refusal fires. Post-rebind behavior is **NOT DETERMINED** because no rebind exists yet.
- The inferred untracked-child-of-non-default-ancestor write path has source evidence but no runtime test. Task 1 creates that test before changing behavior.
- Live host behavior varies. Task 6 requires a real host matrix rather than inferring recovery from repository code.
- MCP 2026-07-28 is a real compatibility gap. No evidence connects it to the branch-attachment symptom. The companion plan handles it separately.

## File and Responsibility Map

- `src/tokensave/staleness.rs`: classify live branch attachment and block unsafe syncs.
- `src/tokensave/query.rs`: expose stable served and working-branch diagnostics.
- `src/mcp/server.rs`: own the swappable graph, automatic tracked-branch rebind, explicit reconnect dispatch, status data, and recovery instructions.
- `src/mcp/tools/definitions.rs`: register an always-loaded, state-changing reconnect tool.
- `src/servers.rs`: refresh the current process registry entry and record executable fingerprints.
- `src/cli.rs`, `src/main.rs`: add `serve --auto-track` and apply it before opening the graph.
- `src/user_config.rs`, `src/agents/mod.rs`: version the generated integration configuration independently of the package version.
- `src/agents/integrations/*.rs`: add `--auto-track` to every generated local MCP command.
- `src/doctor.rs` and integration health checks: report config revision, direct-binary freshness, wrapper uncertainty, and stale live processes.
- `tests/branch_drift_test.rs`, `tests/auto_sync_bound_test.rs`: preserve existing safety behavior.
- `tests/branch_attachment_test.rs`: cover tracked, shared-default, and unsafe untracked attachment states.
- `tests/reopen_test.rs`: prove explicit and automatic same-process rebind.
- `tests/agent_test.rs`, integration-specific agent tests, `tests/user_config_test.rs`: cover generated configuration and revision refresh.
- `tests/mcp_test.rs`, `tests/mcp_server_test.rs`: cover tool registration, annotations, instructions, and status output.

---

### Task 1: Model live branch attachment and close the untracked sync hole

**Files:**
- Modify: `src/tokensave/staleness.rs:198-283`
- Modify: `src/tokensave/query.rs:798-844`
- Create: `tests/branch_attachment_test.rs`
- Modify: `tests/branch_drift_test.rs`
- Modify: `tests/auto_sync_bound_test.rs`

**Interfaces:**

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchAttachment {
    Current,
    SharedSingleDatabase { working_tree: String },
    TrackedMismatch(BranchDrift),
    Untracked {
        serving: String,
        working_tree: String,
        fallback: String,
    },
}

impl TokenSave {
    pub fn branch_attachment(&self) -> BranchAttachment;
}
```

`SharedSingleDatabase` applies when branch metadata is absent or when metadata identifies only the default database. Both layouts intentionally share one database across branch names. `Untracked` applies only when at least one non-default branch database exists and the working branch has no database. `branch_drift()` remains as a compatibility adapter that returns only `TrackedMismatch`.

- [ ] **Step 1: Add failing attachment-state tests.** Create fixtures for: current tracked branch, tracked-to-tracked checkout, a legacy metadata-free database followed by a checkout, a default-only metadata layout followed by an untracked checkout, and a tracked non-default branch followed by an untracked child checkout. Assert the exact `BranchAttachment` variant for each case. Both single-database layouts must return `SharedSingleDatabase` and continue allowing cross-branch sync.
- [ ] **Step 2: Add the corruption reproducer.** Open an untracked child of a tracked non-default branch, add a child-only file, call `find_stale_files_bounded`, and assert that it refuses rather than returning `AutoSyncScope::Sync`. Read the tracked parent's database afterward and assert that the child-only file is absent.
- [ ] **Step 3: Run the tests and confirm the unsafe case fails.**

```bash
rtk cargo test --features test-transport --test branch_attachment_test --test branch_drift_test --test auto_sync_bound_test
```

Expected before implementation: the untracked multi-database case returns `Sync` or lacks the new enum variant.

- [ ] **Step 4: Implement `branch_attachment`.** Reuse `branch_meta::load_branch_meta`, `branch::current_branch`, and the same ancestor resolution used by `resolve_db_for_branch`. Do not classify a name-only checkout as unsafe when branch metadata contains only the default database.
- [ ] **Step 5: Add `AutoSyncScope::UntrackedBranch` and return it before walking files.** Both startup catch-up and lazy sync already consume `find_stale_files_bounded`; this one change must stop automatic writes into a fallback database.
- [ ] **Step 6: Guard explicit indexing and sync entry points.** In multi-database mode, `TokenSave::sync` and `TokenSave::index_all` must return a configuration error for `Untracked` attachment. The message must name the working branch and `tokensave_reopen` recovery. Single-database tests must remain unchanged.
- [ ] **Step 7: Run the focused tests.** Expected: all attachment, drift, and automatic-sync tests pass, including the tracked-parent database integrity assertion.
- [ ] **Step 8: Inspect the complete diff.** Do not commit without direct authorization.

### Task 2: Make the served graph replaceable and add explicit reconnect

**Files:**
- Modify: `src/tokensave/mod.rs:215-356`
- Modify: `src/mcp/server.rs`
- Modify: `src/mcp/tools/definitions.rs`
- Modify: `src/servers.rs`
- Create: `tests/reopen_test.rs`
- Modify: tests that call `McpServer::cg()`

**Interfaces:**

```rust
pub struct OpenOptions {
    pub auto_track: bool,
}

impl TokenSave {
    pub async fn open_with_options(
        project_root: impl AsRef<Path>,
        options: OpenOptions,
    ) -> Result<Self>;
}

pub struct ReopenOptions {
    pub track_if_missing: bool,
}

#[derive(Debug, Serialize)]
pub struct ReopenOutcome {
    pub old_serving_branch: Option<String>,
    pub new_serving_branch: Option<String>,
    pub working_tree_branch: Option<String>,
    pub tracked_branch_created: bool,
    pub db_path: String,
    pub sync_required: bool,
}

impl McpServer {
    pub async fn reopen_graph(&self, options: ReopenOptions) -> Result<ReopenOutcome>;
}

#[cfg(test)]
pub(crate) struct ReopenTestHooks {
    pub foreground_read_acquired: Arc<tokio::sync::Notify>,
    pub untracked_copy_started: Arc<tokio::sync::Notify>,
    pub allow_progress: Arc<tokio::sync::Notify>,
}
```

Change the server fields to:

```rust
cg: tokio::sync::RwLock<Arc<TokenSave>>,
reopen_lock: tokio::sync::Mutex<()>,
```

Every foreground graph request and every background sync/reindex task must hold a graph read guard for the complete graph operation. For tracked targets, `reopen_graph` takes the write guard only after the replacement is ready. For untracked targets, it takes the write guard before branch creation and holds it through copy, fresh open, HEAD recheck, and swap. These lock orders prevent a swap while old-graph work is running.

- [ ] **Step 1: Write a drift fixture with discriminating branch contents.** Initialize `master` with `master_only.rs` and index it. Create `feature`, remove `master_only.rs`, add and commit `feature_only.rs`, track `feature`, and sync its database. Checkout `master`, open `TokenSave`, construct `McpServer`, and record server identity and the master database path. Only then checkout `feature`. Assert the precondition that the same server still reports the master database and refuses a local graph call for drift. This corrects the superseded plan's fixture, which opened after the feature checkout.
- [ ] **Step 2: Write failing reconnect tests.** Assert: `reopen_graph` succeeds; the same `Arc<McpServer>` reports the feature database; `feature_only` is present; `master_only` is absent; the master database remains unchanged; and a second reconnect is idempotent.
- [ ] **Step 3: Add an untracked recovery test.** In a multi-database project, call `reopen_graph(ReopenOptions { track_if_missing: false })` and expect an actionable error. Repeat with `true`; assert a branch database is created, the server swaps to it, and the ancestor database remains unchanged.
- [ ] **Step 4: Add `OpenOptions`.** Keep `TokenSave::open` as the compatibility wrapper that derives its current config/env behavior. `open_with_options` accepts an explicit `auto_track` value so reconnect can disable implicit branch creation regardless of process environment.
- [ ] **Step 5: Replace direct `self.cg` access.** Acquire read guards in request dispatch, resource handlers, freshness work, version reindex, accounting persistence, and shutdown. Change the test accessor to return an `Arc<TokenSave>` asynchronously. Update every cited test call site.
- [ ] **Step 6: Implement the reopen transaction with one lock order per path.** Serialize reopen attempts with `reopen_lock`. Tracked order: `reopen_lock -> fresh open -> write guard -> HEAD recheck -> swap`. Untracked order: `reopen_lock -> write guard -> track/copy -> fresh open -> HEAD recheck -> swap`. Return a retryable error when active graph work prevents acquiring the write lock within the budget. `reopen_graph` always opens with `OpenOptions { auto_track: false }`; only the write-locked `track_if_missing: true` path may create branch metadata.
- [ ] **Step 7: Refresh graph-scoped server state.** Clear `SessionState::unscanned_shown`; refresh `file_token_map`; reset `last_staleness_check_at` and local age-warning state; reset the version-reindex once-gate for the new database. Preserve process-level call counts and savings counters.
- [ ] **Step 8: Refresh the server registry row.** Add `servers::refresh_current(project_root, db_path)`, preserving PID, start time, and `argv_path`. Extend `re_registering_replaces_rather_than_duplicates` to assert the database path changes without a second row.
- [ ] **Step 9: Do not sync inside the swap.** Return `sync_required: true`; the next ordinary local graph call runs the existing bounded freshness path against the new handle. This keeps the critical section short and preserves the sync budget.
- [ ] **Step 10: Add deterministic concurrency tests.** Use `ReopenTestHooks` and `tokio::sync::Notify` to control ordering. Hold a foreground operation inside its graph read guard and prove reopen cannot swap until it finishes or returns the documented timeout. Repeat with lazy sync and version reindex. Pause an untracked recovery after `track_branch_copy` begins and prove a graph operation cannot start until swap completes. Start two reopen attempts and prove `reopen_lock` serializes them. In every case, assert the ancestor or prior branch database remains unchanged.
- [ ] **Step 11: Run focused tests.**

```bash
rtk cargo test --features test-transport --test reopen_test --test branch_drift_test --test strict_tree_mismatch_test --test multi_mcp_coordination_test --test autosync_budget_test
```

- [ ] **Step 12: Inspect the complete diff.** Do not commit without direct authorization.

### Task 3: Self-heal tracked checkouts and expose Reconnect in every tool catalog

**Files:**
- Modify: `src/mcp/server.rs:1766-2631`
- Modify: `src/mcp/tools/definitions.rs`
- Modify: `src/mcp/tools/handlers/mod.rs`
- Modify: `src/mcp/tools/handlers/info.rs:19-50`
- Modify: `tests/reopen_test.rs`
- Modify: `tests/mcp_test.rs`
- Modify: `tests/mcp_server_test.rs`
- Modify: `tests/strict_tree_mismatch_test.rs`

**Tool contract:**

```json
{
  "name": "tokensave_reopen",
  "title": "Reconnect TokenSave Graph",
  "readOnlyHint": false,
  "anthropic/alwaysLoad": true,
  "input": {
    "track_if_missing": {
      "type": "boolean",
      "default": true
    }
  }
}
```

- [ ] **Step 1: Add `def_rw_always_load`.** Register `tokensave_reopen` with `readOnlyHint: false` and `anthropic/alwaysLoad: true`. The title must contain “Reconnect” so Claude and other clients that render tool titles expose the affordance.
- [ ] **Step 2: Intercept reconnect at server level.** In `handle_tools_call`, route `tokensave_reopen` to `reopen_graph` before selector validation, drift refusal, freshness work, and generic handler dispatch. It must remain callable while every local graph tool is refused.
- [ ] **Step 3: Auto-rebind tracked mismatches.** Before dispatching a local graph tool, inspect `branch_attachment`. For `TrackedMismatch`, call `reopen_graph` with `track_if_missing: false`, then continue the original call against the new graph. Emit nothing on success.
- [ ] **Step 4: Refuse unsafe untracked attachment.** For `Untracked`, return an error that names the working and served branches and says: `Call tokensave_reopen with track_if_missing=true to create this branch index and reconnect in-session.` Keep `tokensave_status` and `tokensave_reopen` callable.
- [ ] **Step 5: Make status live.** Add `working_tree_branch`, `serving_branch`, `branch_attachment`, `database_path`, and `reconnect_available` to `tokensave_status`. Compute attachment on every status call; do not reuse startup-cached fallback fields as live state.
- [ ] **Step 6: Update all recovery text.** Replace restart-only text in automatic-sync refusal, strict-tree refusal, default branch refusal, warning banners, and `BASE_INSTRUCTIONS`. State that `tokensave_reopen` reconnects the graph; restarting the host remains a fallback when the tool fails.
- [ ] **Step 7: Pin catalog exposure.** Increase tool counts from 87/86 to 88/87, assert the reconnect tool is always loaded, and add it to the write/exec annotation list. Verify generated Claude permissions include `mcp__tokensave__tokensave_reopen`.
- [ ] **Step 8: Run focused tests.**

```bash
rtk cargo test --features test-transport --test reopen_test --test mcp_test --test mcp_server_test --test branch_drift_test --test strict_tree_mismatch_test
```

- [ ] **Step 9: Inspect the complete diff.** Do not commit without direct authorization.

### Task 4: Make auto-tracking and integration refresh uniform across hosts

**Files:**
- Modify: `src/cli.rs:198-219`
- Modify: `src/main.rs:1120-1188,223-299`
- Modify: `src/user_config.rs`
- Modify: `src/agents/mod.rs:24-94`
- Modify: all MCP command writers in `src/agents/integrations/*.rs`
- Modify: `tests/agent_test.rs`
- Modify: integration-specific agent tests
- Modify: `tests/user_config_test.rs`

**Interfaces:**

```rust
pub const AGENT_CONFIG_REVISION: u32 = 1;

// Machine-local state, persisted in ~/.tokensave/state.toml.
pub agent_config_revision: u32;

pub fn discover_configured_integrations(
    home: &Path,
    installed_agents: &mut Vec<String>,
) -> Vec<String>;
```

The generated command becomes `tokensave serve --auto-track`. Using a CLI argument avoids incompatible per-host environment-field schemas and works with every existing `{command,args}` or command-array writer.

- [ ] **Step 1: Add `--auto-track` to serve.** `Commands::Serve { auto_track: true }` calls Task 2's `TokenSave::open_with_options(..., OpenOptions { auto_track: true })` during initial server open. Do not use a process-wide environment mutation. Task 2's `reopen_graph` always uses `auto_track: false`, so `track_if_missing: false` cannot create a database indirectly.
- [ ] **Step 2: Add failing installer tests for all 21 integrations.** Each generated TokenSave MCP command must contain `serve` followed by `--auto-track`, preserving each host's existing command shape and unrelated fields.
- [ ] **Step 3: Update every MCP writer.** Standard JSON writers use `args: ["serve", "--auto-track"]`; command-array writers append the flag; TOML and nested-command writers preserve their native shape. Do not add an unverified `env` key.
- [ ] **Step 4: Discover configured integrations before revision maintenance.** Scan every integration with its existing detection/config parser, merge missing IDs into `installed_agents`, persist the merged list, and only then evaluate `AGENT_CONFIG_REVISION`. Do not keep the current early return when `installed_agents` is merely nonempty.
- [ ] **Step 5: Add the independent config revision.** Store `agent_config_revision` in machine-local state. Make `resync_installed_agents` run when the stored revision differs from `AGENT_CONFIG_REVISION`, even when `CARGO_PKG_VERSION` is unchanged or only a patch changed. Explicit `install` and `reinstall` also record the current revision after their install loop runs.
- [ ] **Step 6: Advance the revision only after the install loop runs.** Preserve current one-time failure reporting: collect failed integrations and report them, without retrying on every command.
- [ ] **Step 7: Preserve wrapper commands.** Keep `preserve_mcp_command`; verify that a wrapper receives `serve --auto-track`. A wrapper whose child binary cannot be proven current remains a doctor warning in Task 5.
- [ ] **Step 8: Add propagation-boundary tests.** Prove initial `serve --auto-track` tracks an untracked startup branch. Then checkout another untracked branch and call reopen with `track_if_missing: false`; assert no metadata or database appears. Repeat with `true`; assert creation and swap occur under the exclusive transaction. Add empty and partial `installed_agents` fixtures with pre-existing configurations for every supported command shape; a same-semver revision mismatch must discover and refresh each integration.
- [ ] **Step 9: Run installer and state tests.**

```bash
rtk cargo test --test agent_test --test user_config_test --test omp_agent_test
```

- [ ] **Step 10: Inspect the complete diff.** Do not commit without direct authorization.

### Task 5: Measure configured-binary and live-process freshness

**Files:**
- Modify: `src/servers.rs`
- Modify: `src/doctor.rs`
- Modify: shared integration health-check helpers and Claude's `doctor_check_mcp_binary`
- Modify: `src/mcp/tools/handlers/info.rs`
- Modify: server-registry, doctor, and handler tests

**Registry contract extension:**

```rust
#[serde(default)]
pub binary_sha256: Option<String>;
```

- [ ] **Step 1: Fingerprint the executable at server startup.** Reuse the existing `sha2` dependency. Hash `std::env::current_exe()` once during registration and store the lowercase SHA-256 value in `ServerEntry`. Keep the field optional so old registry rows deserialize.
- [ ] **Step 2: Surface build identity.** Add `binary_sha256` to `tokensave servers --json`, `tokensave_status`, and human server listings. Keep `version` because package version and executable identity answer different questions.
- [ ] **Step 3: Compare live processes with the current executable.** Doctor hashes its own executable and warns for every live TokenSave server whose recorded hash differs. Include PID, project path, and the exact recovery: host process restart or the host's native process reconnect. Do not call graph rebind a binary refresh.
- [ ] **Step 4: Report configured-command certainty.** Direct binary commands can be canonicalized and hashed. Wrapper commands must report `version provenance: not verified through wrapper` unless the wrapper exposes an explicit child path. Do not mark name/path existence as version freshness.
- [ ] **Step 5: Report integration config revision.** Doctor compares stored `agent_config_revision` with `AGENT_CONFIG_REVISION` and directs the user to `tokensave reinstall` when stale.
- [ ] **Step 6: Add backward-compatibility and mismatch tests.** Cover registry JSON without `binary_sha256`, matching and mismatching hashes, wrapper uncertainty, and config revision mismatch.
- [ ] **Step 7: Run focused tests.**

```bash
rtk cargo test --test agent_test --test mcp_handler_test
rtk cargo test servers::tests doctor::tests
```

- [ ] **Step 8: Inspect the complete diff.** Do not commit without direct authorization.

### Task 6: Update user guidance and verify real host behavior

**Files:**
- Modify: `docs/BRANCHING-USER-GUIDE.md:110-138`
- Modify: `CHANGELOG.md`
- Modify: generated agent-rule sources if they mention restart-only recovery

- [ ] **Step 1: Update branch guidance.** Explain automatic tracked-branch rebind, explicit `tokensave_reopen`, untracked branch tracking, and restart fallback. Keep graph rebind separate from process restart and configuration reload.
- [ ] **Step 2: Update release notes.** Record the reconnect tool, automatic tracked rebind, untracked-branch refusal, `serve --auto-track`, config revision, and live executable fingerprint.
- [ ] **Step 3: Build and install to an isolated root.**

```bash
rtk cargo install --path . --locked --force --root /var/folders/48/26v5cm5n743d_md4tl44jr9c0000gp/T/kilo/tokensave-plan-smoke
```

- [ ] **Step 4: Verify generated configurations in temporary homes.** Run installer tests rather than touching real host configuration. Confirm all 21 integrations include `serve --auto-track` and expose the reconnect tool through generated permissions where the host has permissions.
- [ ] **Step 5: Run the same-process branch smoke through a test transport.** Start on tracked branch A, checkout tracked branch B, issue a normal graph call, and confirm automatic rebind, unchanged server identity, B's database path, and B-only results.
- [ ] **Step 6: Run the untracked branch smoke.** Checkout untracked branch C in a multi-database project. Confirm graph calls refuse with no ancestor write. Call `tokensave_reopen` with tracking enabled and confirm the same process serves C afterward.
- [ ] **Step 7: Run live host checks only after explicit approval to alter local installations.** Record results separately for Claude Code/Desktop, Kilo, Codex, Cursor, Gemini, and Copilot when installed. For each host record tool visibility, automatic tracked rebind, explicit untracked recovery, PID continuity, configured command, live executable hash, and recovery latency. A repository test cannot prove host behavior.
- [ ] **Step 8: Screen the changed documentation.** Run the technical-writing screen against the guide and changelog, excluding code blocks from interpretation.
- [ ] **Step 9: Inspect the complete diff.** Do not commit without direct authorization.

## Final Verification

Run the focused suites first, then the isolated full suite:

```bash
rtk cargo fmt --all -- --check
rtk cargo test --features test-transport --test branch_attachment_test --test reopen_test --test branch_drift_test --test strict_tree_mismatch_test --test auto_sync_bound_test --test autosync_budget_test --test mcp_test --test mcp_server_test
rtk cargo test --test agent_test --test user_config_test --test omp_agent_test
rtk env XDG_CONFIG_HOME="/var/folders/48/26v5cm5n743d_md4tl44jr9c0000gp/T/kilo/tokensave-empty-xdg" GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1 cargo test --workspace --locked
rtk cargo clippy --workspace --all-targets --locked -- -D warnings
```

The plan passes only when all of these observations hold:

1. A tracked checkout self-heals on the next local graph call without a new process.
2. An untracked checkout in multi-database mode never returns an ancestor-branch answer and never writes into the ancestor database.
3. `tokensave_reopen` is visible as **Reconnect TokenSave Graph**, remains callable during refusal, and can explicitly track and rebind an untracked branch.
4. Single-database projects retain current cross-branch sync behavior.
5. Every generated host command includes `serve --auto-track`.
6. Same-version source builds refresh agent configuration when `AGENT_CONFIG_REVISION` changes.
7. Doctor distinguishes package version, configured executable certainty, and live executable hash.
8. The live host matrix names every client that was not tested. Untested clients remain **NOT VERIFIED**.
9. Barrier-controlled tests prove tracked reopen, lazy sync, version reindex, untracked branch creation, and concurrent reopen attempts obey the documented lock order without writing the prior database.
10. Revision maintenance discovers pre-existing integrations missing from `installed_agents` and refreshes every supported command shape on a same-semver config revision change.

## Delivery Gate

No fork exists and the local feature branch has no upstream. After Bobby explicitly authorizes repository creation, remote changes, and pushing, create `bobbypierce42/tokensave`, rename the developer remote to `upstream`, add the fork as `origin`, and push only `feat/branch-reopen-and-mcp-latest`. Bobby alone decides whether anything merges.

## Decision Required

Live installation changes, fork creation, remote changes, commits, and pushes each require separate authorization. Please approve Tasks 1 through 5 for implementation on `feat/branch-reopen-and-mcp-latest`.
