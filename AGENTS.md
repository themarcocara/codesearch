# AGENTS.md — codesearch (feature/tui-rm-worktree-hook)

## Current state

- **Branch:** `feature/tui-rm-worktree-hook` (based on `develop` at 7931749)
- **Parent repo:** `C:\WorkArea\AI\codesearch\codesearch.git` (on `feature/host-binding`)
- **Version:** v1.0.178
- **Status:** `cargo check` + `cargo clippy` clean (starting point)
- **Validation:** `cargo check` for iteration, `cargo clippy` for lint. No `--release` builds.

## Features for this branch

### Feature 1: TUI 'r' key — Remove DB with Confirmation Dialog

**Goal:** Pressing `'r'` in the TUI shows a confirmation dialog "Delete <alias>?". On confirm (Enter/y), the repo's index is removed (FSW stopped, evicted from memory, unregistered from repos.json, .codesearch.db deleted). On cancel (Esc), dialog dismisses.

**Implementation steps (in order):**

| Step | File | What |
|------|------|------|
| 1 | `src/serve/tui_common.rs` | Add `KeyAction::RequestRemove(usize)` + `KeyAction::ConfirmRemove` to `KeyAction` enum (line 45-57) |
| 2 | `src/serve/tui_common.rs` | Add `OverlayState::ConfirmRemove { alias: String }` to `OverlayState` enum (line 60-78) |
| 3 | `src/serve/tui_common.rs` | Add `'r'` → `RequestRemove(idx)` in `handle_key()` (line 87-130) |
| 4 | `src/serve/tui_common.rs` | New `OverlayKeyAction` enum + `handle_overlay_key()` function. Returns: `Dismiss` (Esc), `ConfirmRemove` (Enter/y for ConfirmRemove overlay), `None` (all other keys). This replaces the inline overlay key handling in both event loops. |
| 5 | `src/serve/tui.rs` + `src/serve/tui_remote.rs` | Replace inline overlay key handling (tui.rs:160-166, tui_remote.rs:217-223) with `handle_overlay_key()` |
| 6 | `src/serve/tui_common.rs` | Add `ConfirmRemove` rendering in `render_overlay()` (line 525-627) — red-bordered warning modal with "⚠ Delete <alias>?" and "[Enter] confirm [Esc] cancel" |
| 7 | `src/serve/tui_common.rs` | Add `[r] remove` span to footer (line 499-504, after `[f] reindex`) |
| 8 | `src/serve/mod.rs` | Extract `ServeState::remove_repo(&self, alias: &str)` from `remove_repo_handler()` (line 2838-2958). Logic: resolve path from config → stop_fsw + evict from DashMaps → unregister from config + persist → delete .codesearch.db with 5 retries at 300ms. The handler then just calls this method. |
| 9 | `src/serve/tui.rs` | Handle `RequestRemove(idx)` → show `ConfirmRemove` overlay. Handle `ConfirmRemove` → spawn tokio task calling `state.remove_repo(&alias)`. Also handle `OverlayKeyAction::ConfirmRemove` from `handle_overlay_key()`. |
| 10 | `src/serve/tui_remote.rs` | Handle `RequestRemove(idx)` → show `ConfirmRemove` overlay. Handle `ConfirmRemove` → HTTP `DELETE /repos/{alias}`. Also handle `OverlayKeyAction::ConfirmRemove` from `handle_overlay_key()`. |

**Key architectural details:**

- **KeyAction enum** (tui_common.rs:45-57): `None`, `Reload`, `ShowInfo(usize)`, `RunDoctor(usize)`, `ForceReindex(usize)`
- **OverlayState enum** (tui_common.rs:60-78): `Info{...}`, `DoctorRunning{alias}`, `Doctor{alias, results}`
- **handle_key()** (tui_common.rs:87-130): maps keys to KeyAction. `'r'` is currently unbound (falls to `_ => None`)
- **Overlay key blocking** (tui.rs:160-166, tui_remote.rs:217-223): `if overlay.is_some() { if Esc { overlay=None; } continue; }` — ALL keys blocked except Esc. Must be changed to also allow Enter/y for ConfirmRemove.
- **render_centered_modal()** (tui_common.rs:633-679): generic modal renderer, accepts title + Vec<Line>. Reusable for confirmation.
- **render_overlay()** (tui_common.rs:525-627): match on OverlayState variants, calls render_centered_modal()
- **render_footer()** (tui_common.rs:459-517): left_line has spans for keybindings
- **remove_repo_handler** (mod.rs:2838-2958): resolve path → stop_fsw → evict from repos/last_access DashMaps → unregister from config + persist → delete DB with 5 retries at 300ms
- **stop_fsw()** (mod.rs:1090): returns Option<Arc<SharedStores>>
- **ServeState fields:** config (RwLock<ReposConfig>), repos (DashMap<String, RepoState>), last_access (DashMap)
- **RepoRow struct** (tui_common.rs:23-42): has `alias`, `status`, `changes`, etc.

### Feature 2: Git Worktree Auto-Index via post-checkout Hook

**Goal:** When `git worktree add` creates a new worktree, a git `post-checkout` hook auto-registers it with codesearch serve.

**Implementation steps:**

| Step | File | What |
|------|------|------|
| 11 | `src/serve/mod.rs` | On serve startup, write `~/.codesearch/serve_url` with the serve URL (e.g. `http://127.0.0.1:39725`). On shutdown (Drop or cancel), delete the file. |
| 12 | `src/cli/mod.rs` | Add `codesearch hook install` subcommand that writes a `post-checkout` hook script into `.git/hooks/`. The hook reads `~/.codesearch/serve_url` and POSTs the worktree path to `POST /repos`. |
| 13 | Inline in hook script | Script template (bash + PowerShell): reads serve_url file, checks if it's a worktree checkout (prev_head ≠ new_head and branch flag = 1), then POSTs the working directory to serve |
| 14 | Update AGENTS.md | Document worktree workflow |

**Hook template logic:**
```bash
#!/bin/bash
# codesearch post-checkout hook
# $1 = prev_ref, $2 = new_ref, $3 = flag (1=branch checkout)
SERVE_URL_FILE="$HOME/.codesearch/serve_url"
if [ -f "$SERVE_URL_FILE" ]; then
    SERVE_URL=$(cat "$SERVE_URL_FILE")
    curl -s -X POST "$SERVE_URL/repos" -H "Content-Type: application/json" \
      -d "{\"path\":\"$(pwd)\"}" &>/dev/null &
fi
```

**Key details:**
- `POST /repos` endpoint already exists (add_repo_handler, mod.rs ~line 2597)
- `ReposConfig::register()` handles dedup (won't re-register existing path)
- serve_url file is simple: just the URL string, one line
- Hook must be executable (`chmod +x`)
- For Windows: also provide a `.ps1` version or handle in the bash script via Git Bash

### Bug Fix: `.git/info/exclude` broken for worktrees

**Problem:** `src/watch/mod.rs:93` — `root.join(".git").join("info").join("exclude")` fails for worktrees because `.git` is a file (containing `gitdir: <path>`), not a directory.

**Fix:** Add `resolve_git_dir(root: &Path) -> PathBuf` helper to `FileWatcher` (same file, before `build_gitignore`). Logic:
1. Check if `root.join(".git")` is a file
2. If yes: read it, parse `gitdir: <path>` from first line, resolve relative to root
3. If no: return `root.join(".git")`

Then in `build_gitignore()` (line 93), replace:
```rust
let exclude_path = root.join(".git").join("info").join("exclude");
```
with:
```rust
let git_dir = Self::resolve_git_dir(root);
let exclude_path = git_dir.join("info").join("exclude");
```

## Architecture (relevant parts only)

### TUI files
- `src/serve/tui.rs` — Embedded TUI (runs in serve process, direct Arc<ServeState> access)
- `src/serve/tui_common.rs` — Shared types + rendering (KeyAction, OverlayState, RepoRow, all render_ functions)
- `src/serve/tui_remote.rs` — Remote TUI (connects via HTTP to running serve)

### Key types
- `ServeState` (src/serve/mod.rs) — config: RwLock<ReposConfig>, repos: DashMap<String, RepoState>, last_access: DashMap
- `ReposConfig` (src/db_discovery/repos.rs) — repos: HashMap<String, PathBuf>, groups, repos_meta
- `SharedStores` — wraps VectorStore + FTS, shared via Arc

### Existing remove flow
- HTTP: `DELETE /repos/:alias` → `remove_repo_handler()` (mod.rs:2838)
- CLI: `remove_from_index()` (src/index/mod.rs:1614)
- Both: stop FSW → evict from memory → unregister from repos.json → delete .codesearch.db (5 retries, 300ms, handles Windows file locks)

## Notes for OpenCode

- **Validation:** `cargo check` and `cargo clippy` for iteration. No `--release` builds — always dev/debug.
- **Runtime:** `C:\Users\develterf\.local\bin\` — `codesearch.exe` + `helpers/csharp/scip-csharp.exe`
- **Build:** `target/release/` — outside repo (via `CARGO_TARGET_DIR`)
- **Deploy:** `..\copy-to-common.ps1` — builds + copies both binaries to `~/.local/bin/`
- **Canonical paths:** NEVER call `.canonicalize()` directly. Always use `safe_canonicalize()`.
- **LMDB rule:** No two `EnvOpenOptions::open()` on same dir in same process. All access via `get_or_open_stores()` → `Arc<SharedStores>`.
