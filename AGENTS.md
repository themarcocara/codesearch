# AGENTS.md — codesearch (feature/global-codesearchignore)

## Current state

- **Branch:** `feature/global-codesearchignore` (based on `develop` at 7b8cd71)
- **Version:** v1.0.189
- **Status:** `cargo check` + `cargo clippy` clean
- **Validation:** `cargo check` for iteration, `cargo clippy` for lint. No `--release` builds.

## Features for this branch

Addresses GitHub Issue #115 (flupkede/codesearch).

### Feature 1: Global `.codesearchignore` + FileWatcher bug fix

**Status:** ✅ Done (commit 4cbfa57)

- `~/.codesearch/.codesearchignore` — global ignore file with lowest priority
- FileWalker (`src/file/mod.rs`) loads it via `WalkBuilder::add_ignore()`
- FileWatcher (`src/watch/mod.rs`) loads it in `build_gitignore()` alongside repo-local `.codesearchignore` (which was previously missing — bug fix)
- Precedence: global < .git/info/exclude < .gitignore < repo-local .codesearchignore

### Feature 2: Jupyter Notebook (.ipynb) support

**Status:** ✅ Done (commits 67ec214, 7d96538)

- `Language::Jupyter` variant added to enum, `"ipynb"` extension mapped
- `src/chunker/jupyter.rs` — custom cell extraction (no tree-sitter):
  - Parses .ipynb JSON via serde_json
  - Extracts code and markdown cells
  - Tags chunks with `# [code]` / `# [markdown]` prefix
  - Merges adjacent same-type cells < 50 lines
  - Malformed JSON → `warn!` log + empty Vec
- Integrated in `semantic.rs` alongside Markdown special-case path
- 9 unit tests passing

## Architecture (relevant parts only)

### Ignore pipeline

**FileWalker** (src/file/mod.rs): Uses `ignore::WalkBuilder` with built-in gitignore + custom filenames:
- `.gitignore`, `.git/info/exclude`, global gitignore (via git config)
- `.codesearchignore`, `.osgrepignore` (repo-local)
- `~/.codesearch/.codesearchignore` (global, via `add_ignore()`)

**FileWatcher** (src/watch/mod.rs): Manually builds `Gitignore` matcher from:
- `~/.codesearch/.codesearchignore` (global, lowest priority)
- `.git/info/exclude` (worktree-aware via `resolve_git_dir()`)
- `.gitignore` (repo root)
- `.codesearchignore` (repo-local, highest priority)

### Jupyter chunker pipeline

`chunk_semantic()` → `Language::Jupyter` → `jupyter::chunk_jupyter()` → JSON parse → cell extraction → merge → chunk creation

### Key constants

- `GLOBAL_CODESEARCHIGNORE_FILE` = `".codesearchignore"` (src/constants.rs)
- `global_codesearchignore_path()` → `~/.codesearch/.codesearchignore` (src/constants.rs)
- `MERGE_LINE_LIMIT` = 50 (src/chunker/jupyter.rs)

## Notes for OpenCode

- **Validation:** `cargo check` and `cargo clippy` for iteration. No `--release` builds — always dev/debug.
- **Runtime:** `C:\Users\develterf\.local\bin\` — `codesearch.exe` + `helpers/csharp/scip-csharp.exe`
- **Build:** `target/release/` — outside repo (via `CARGO_TARGET_DIR`)
- **Deploy:** `..\copy-to-common.ps1` — builds + copies both binaries to `~/.local/bin/`
- **Canonical paths:** NEVER call `.canonicalize()` directly. Always use `safe_canonicalize()`.
- **LMDB rule:** No two `EnvOpenOptions::open()` on same dir in same process. All access via `get_or_open_stores()` → `Arc<SharedStores>`.
