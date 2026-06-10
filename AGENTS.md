# AGENTS.md — codesearch

## Current state

- **Branch:** `develop`
- **Version:** v1.0.178
- **Status:** `cargo check` + `cargo clippy` clean
- **Last deployed:** v1.0.124 via `..\copy-to-common.ps1`

## GitHub Issues — Backlog (2026-06-10)

### #115 — More codesearchignore options → ASKED FOR JUSTIFICATION (polite reply posted)

User wants external ignore files for cloned dependencies. Verified that `.gitignore` (repo-local, global `~/.gitignore`, `.git/info/exclude`) and `.codesearchignore` both work hierarchically via `ignore` crate. Posted polite comment asking for concrete example. If niche → won't fix.

### #118 — `--model` flag silently ignored by `index add` → BUG FIX (priority: next)

**Bug:** Global `--model` CLI flag exists but is silently ignored by `index add`. Every repo gets default model (`AllMiniLML6V2Q`).

**Root cause — 6 gaps in the plumbing:**

| # | Gap | File:Line | Fix |
|---|-----|-----------|-----|
| 1 | `IndexCommands::Add` has no `--model` field | `src/cli/mod.rs:16-23` | Add `model: Option<String>` field |
| 2 | `add_to_index()` has no `model` param | `src/index/mod.rs:1429` | Add `model: Option<ModelType>` param |
| 3 | CLI dispatch ignores `model_type` at Add | `src/cli/mod.rs:537-540` | Pass `model_type` to `add_to_index()` |
| 4 | `try_delegate_add_to_serve()` omits model from HTTP body | `src/index/mod.rs:2191` | Add `model` to JSON body |
| 5 | `AddRepoRequest` has no `model` field | `src/serve/mod.rs:2583-2589` | Add `model: Option<String>` |
| 6 | `force_reindex_with_stores()` defaults to hardcoded model | `src/index/manager.rs:776` | Add `model_override: Option<ModelType>` |

**Thread:** `ModelType::parse(s)` (src/embed/embedder.rs:177) → `add_to_index(path, global, model, cancel_token)` → both `index()` calls pass `model` instead of `None` → `try_delegate_add_to_serve(path, alias, global, model)` includes model in JSON → `AddRepoRequest.model` → `add_repo_handler` parses via `ModelType::parse()` → `force_reindex_with_stores(path, db, stores, model_override)`.

**Implementation plan:**
1. Add `model: Option<String>` to `IndexCommands::Add` variant
2. Update Add dispatch (line 537) to parse model and pass through
3. Update backward-compat add path (line 561-563) similarly
4. Add `model: Option<ModelType>` param to `add_to_index()` (line 1429)
5. Pass `model` instead of `None` in both `index()` calls (lines 1562, 1575)
6. Add `model: Option<ModelType>` param to `try_delegate_add_to_serve()` (line 2150)
7. Include model in JSON body: `body["model"] = model.map(|m| m.short_name().to_string())`
8. Add `model: Option<String>` to `AddRepoRequest` struct (line 2583)
9. Parse model in `add_repo_handler` (line 2597), write metadata.json with chosen model before force_reindex
10. Add `model_override: Option<ModelType>` to `force_reindex_with_stores()` (line 751)

**Branch:** `fix/model-flag-ignored`

### #114 — Let serve also bind non-localhost → FEATURE + SECURITY

**Problem:** `codesearch serve` hardcodes `127.0.0.1` in 7 locations. No `--host` flag or env var.

**Feature (5 files, ~60 lines):**

| # | Location | Current | Fix |
|---|----------|---------|-----|
| 1 | `src/constants.rs` | No host constants | Add `DEFAULT_SERVE_HOST = "127.0.0.1"`, `SERVE_HOST_ENV = "CODESEARCH_SERVE_HOST"` |
| 2 | `src/cli/mod.rs:258` | `Serve` has `--port` only | Add `--host` flag |
| 3 | `src/serve/mod.rs:3184` | `([127,0,0,1], port)` | Dynamic `SocketAddr` from host+port |
| 4 | `src/serve/mod.rs:3142` | `run_serve(port, ...)` | Add `host: Option<String>` param |
| 5 | 5 URL helpers | `"http://127.0.0.1:{port}"` | Use `serve_host()` helper (env→default) |

**URL helpers that need `serve_host()`:**
- `serve_base_url()` — `src/cli/mod.rs:382`
- `serve_url_from_env()` — `src/mcp/mod.rs:2240`
- `try_delegate_reindex_to_serve()` — `src/index/mod.rs:1947`
- `try_delegate_add_to_serve()` — `src/index/mod.rs:2162`
- `try_delegate_rm_to_serve()` — `src/index/mod.rs:2243`

**Security additions:**
- Startup check: if host ≠ `127.0.0.1`/`::1`/`localhost` → **refuse** without `CODESEARCH_SERVE_API_KEY`
- New `require_auth_for_network` middleware: when non-localhost, protect ALL routes (including MCP) with API key
- Same Bearer/X-API-Key pattern as existing `require_admin_auth` (src/serve/mod.rs:3033)
- Existing `require_admin_auth` stays for management-only auth when localhost with API key

**Key constants:** `DEFAULT_SERVE_PORT: u16 = 39725`, `SERVE_PORT_ENV`, `SERVE_API_KEY_ENV`, `DEFAULT_SERVE_URL = "http://127.0.0.1:39725"` — all in `src/constants.rs`.

**Current router setup:** Lines 3237-3250 — routes, layers: `log_mcp_requests` → `require_admin_auth` → handler.

**Branch:** `feature/host-binding`

## Implemented Features

- **`find_impact` MCP tool** — returns transitive call-sites for a symbol (name/position-based), C# via `scip-csharp`
- **`scip-csharp` helper** — .NET 10 CLI wrapping Roslyn. Subcommands: `index` (definitions only), `find-refs` (lazy, per-symbol), `batch-find-refs` (Phase 3 pre-warm)
- **Lazy reference resolution** — rebuild stores definitions only; refs resolved on first `find_impact`, cached in `scip_ref_cache` LMDB
- **Incremental merge** — `RebuildScope::Files`: reverse-maps stale symbol keys, merges new defs (partial-class safe)
- **O(1) lookups** — `scip_positions` (file:line → symbols), `scip_simple_names` (identifier → full keys)
- **Startup phases** — Phase 1: text/vector warmup; Phase 2: C# SCIP definitions (gated by `Semaphore(CSHARP_SCIP_CONCURRENCY)`); Phase 3: batch ref cache pre-warm
- **`repos_meta` tracking** — `RepoMeta` persisted in `repos.json`, stale-path resilience with auto-relocation
- **Alias always derived** — directory name via `ReposConfig::register()`, no user override
- **TUI C# indicator** — green `C#·` ready, yellow `C#…` indexing, red `C#!` error
- **SCIP LMDB map_size 512 MB** — override with `CODESEARCH_SCIP_LMDB_MAP_MB`
- **Watcher .csproj grouping** — incremental rebuild per project instead of full solution

## Architecture

### Per-language adapter pattern

`src/symbols/` hosts the adapter layer:

- `mod.rs` — `SymbolIndexer` trait + `SymbolIndexerRegistry` dispatch
- `csharp.rs` — C# adapter (rebuild, find_references, find_references_by_position)
- `scip_parse.rs` — JSON parser for scip-csharp output

### LMDB tables

| Table | Key | Value |
|---|---|---|
| `scip_symbols` | full SCIP key | definitions only (after lazy-refs refactor) |
| `scip_positions` | `<file>:<line>` | `[symbol_keys]` |
| `scip_simple_names` | last segment identifier | `[full_keys]` |
| `scip_ref_cache` | full SCIP key | lazy-resolved references |
| `scip_meta` | `last_rebuild_ts`, `symbol_count` | `Str` |

### Helper detection

1. `CODESEARCH_SCIP_CSHARP` env var
2. `<codesearch-exe-dir>/helpers/csharp/scip-csharp[.exe]`
3. `$PATH`

### Startup phases

| Phase | What | Gating |
|---|---|---|
| Phase 1 | Sequential text/vector warmup | `run_phase_1_warmup_all()` |
| Phase 2 | C# SCIP definitions-only | `Semaphore(CSHARP_SCIP_CONCURRENCY, default 2)` |
| Phase 3 | Batch reference cache pre-warm | `CSHARP_PREWARM_ENABLED` (default: true) |

### `SymbolIndexerRegistry` ownership

4 `Arc::new(SymbolIndexerRegistry::new())` sites: `IndexManager::new()`, `IndexManager::new_for_path()`, `ServeState::new()`, `CodesearchService::new_with_stores()`. `CodesearchService::new_for_serve()` clones from `ServeState`.

## Known Bugs

### Bug 1 — `.gitignore` not respected → ✅ FIXED

Was: build artifacts indexed as source. Fixed — `FileWalker` now uses `WalkBuilder` with `git_ignore(true)`, `git_global(true)`, `git_exclude(true)`, `add_custom_ignore_filename(".codesearchignore")` (src/file/mod.rs:95-99).

### Bug 2 — MSBuildWorkspace picks up `obj/` generated files (HIGH — scip-csharp)

**Workaround:** Add `Directory.Build.props` at solution root with `<Compile Remove="obj\**" />`.
**Proper fix:** Pass `DesignTimeBuild=true` + `SkipCompilerExecution=true` MSBuild properties in scip-csharp.

### Bug 3 — `--filter-project` selects wrong project when workspace fails to load (MEDIUM)

When a .csproj fails to load, changed files are silently reassigned to a sibling that did compile. No warning. Fix: log warning + don't reassign.

### Field-tested bugs (2026-05-08)

| ID | Severity | Title | Status |
|----|----------|-------|--------|
| B1 | 🔴 CRITICAL | Double chunks (2× indexed) in one repo | Fix: `codesearch index -f <alias>` |
| B2 | 🔴 CRITICAL | `status(kind="projects")` reports 0 chunks | Open — aggregation bug |
| B3 | 🟡 MEDIUM | Regex `\w+`/`\b` doesn't work in literal mode | Open — BM25 tokenizes before regex |
| B4 | 🟡 MEDIUM | Duplicate defs in find_impact (src/ prefix) | Open — path dedup needed |
| B5 | 🟠 LOW | JS noise in `find definition` group-scope | Open — language filter |
| B6 | 🟠 LOW | BM25 prefix-matching (TestPlan ≠ TestPlanCache) | Open — need subword tokenization |
| B7 | 🔴 HIGH | Stale chunks after delete (caused by B1) | Fixed by B1 force reindex |

## ⚠️ Canonical Path Policy — MANDATORY

**NEVER call `.canonicalize()` directly. Always use `safe_canonicalize()`.**

```rust
// ❌ FORBIDDEN
let p = path.canonicalize()?;
// ✅ REQUIRED
use crate::cache::safe_canonicalize;
let p = safe_canonicalize(path)?;
```

Defined in `src/cache/file_meta.rs`, exported via `crate::cache`. Calls `canonicalize()` then `strip_unc_prefix()`.

### ⚠️ LMDB Access Rule — CRITICAL

LMDB does not allow two `EnvOpenOptions::open()` handles on the same directory in the same process.

- **Serve context:** ALL LMDB access through `get_or_open_stores()` → `Arc<SharedStores>`
- **Forbidden:** `VectorStore::new()`, `VectorStore::open_readonly()`, direct `heed::EnvOpenOptions::open()` on `.codesearch.db`
- **Allowed in CLI/stdio:** `VectorStore::new()` is fine (own process)

**4 LMDB environments:**
1. Vector DB — `.codesearch.db/` via `VectorStore`
2. SCIP — `.codesearch.db/scip/` via `open_scip_env()` (separate dir)
3. Embed cache — `~/.codesearch/embed_cache/` via `EmbeddingCache`
4. FTS — `.codesearch.db/fts/` — Tantivy (NOT LMDB)

## Remaining work

- [ ] **Warmup blocks tokio runtime** — sync I/O in `perform_incremental_refresh_with_stores` starves executor. Fix: `spawn_blocking`
- [ ] Verify cached `find_impact` < 100ms (currently 216ms via HTTP)
- [ ] CI green on `csharp-integration-tests` job
- [ ] Warn if `--filter-project` passed to `find-refs` CLI
- [ ] Standalone `index symbol` without serve

## Notes for OpenCode

- **Validation:** `cargo check` and `cargo clippy` for iteration. **No `--release` builds — always dev/debug.**
- `scip-csharp` is self-contained single-file .NET 10 publish (no runtime required)
- Symbol resolution: exact match first, then fuzzy via `scip_simple_names`
- Position lookup matches `start_line` only

### Runtime vs build locations

- **Runtime:** `C:\Users\develterf\.local\bin\` — `codesearch.exe` + `helpers/csharp/scip-csharp.exe`
- **Build:** `target/release/` — outside repo (via `CARGO_TARGET_DIR`). For compilation only.
- **Logs:** `~\.codesearch\logs\` — structured logs during serve

### Deploying to runtime

- `..\copy-to-common.ps1` — builds + copies both binaries to `~/.local/bin/`
- Helper: `dotnet publish helpers/csharp/scip-csharp.csproj -r win-x64 --self-contained -c Release`
- Only copy single `.exe` — no DLLs, no `BuildHost-*` dirs

## Release workflow — `/merge` and `/release`

- **`/merge`** — land feature branch on `develop`: checks → commit → validate → push → PR → auto-merge
- **`/release`** — `/merge` + promote `develop` → `master` via PR + push tag → triggers release workflow (6 archives)
- **Version rule:** pre-commit hook bumps patch only on `feature/*`/`fix/*` branches; `develop`/`master` get `cargo fmt` only
