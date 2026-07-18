# Handoff: Haxe `find_impact` support (Tier A — compiler-accurate references)

**Prerequisite: this document assumes `HAXE_INTEGRATION_HANDOFF.md` (Phase 1 —
semantic chunking) is already implemented and merged.** `Language::Haxe`
exists, `.hx` files chunk correctly via `themarcocara/tree-sitter-haxe`. This
document is a separate, independent feature: wiring Haxe into `find_impact`
(`src/symbols/`) with real type-checked "find references," not the syntactic
best-effort tier tree-sitter/haxeparser would give you.

Read `AGENTS.md` and `.claude/CLAUDE.md` first, as always. This doc does not
repeat their branching/validation rules except where directly relevant.

## Why this document exists (don't re-litigate this)

Two weaker options were considered and rejected as the primary path:
- **tree-sitter-haxe** (already added for chunking in Phase 1): purely
  syntactic, no type/overload/import resolution. Fine for chunk boundaries,
  not for "find all references."
- **`HaxeCheckstyle/haxeparser`**: a better syntactic parser than
  tree-sitter — written using the same parser-combinator lineage as the real
  compiler, producing the actual `haxe.macro.Expr` AST types used throughout
  the Haxe ecosystem — but it is *still* purely syntactic. No type checker.
  It would reduce false positives in a heuristic reference-matcher (real
  `import`/`using` AST nodes to scope candidates) but cannot give
  type-resolved references. Useful as a *fallback* tier, never as the
  primary answer.

The only thing that actually resolves types in the Haxe ecosystem is the
**Haxe compiler itself**, via its `--display` / completion protocol. That
protocol is what powers `haxe-language-server` (the backend of the vshaxe
VS Code extension), and `haxe-language-server` implements standard LSP,
including `textDocument/references` — confirmed present as
`FindReferencesFeature.hx` in that repo. That is the target for this task:
drive `haxe-language-server` (or the raw `--display` protocol underneath it
— see the spike in Step 0) as a subprocess helper, analogous in spirit to
how `src/symbols/csharp.rs` drives Roslyn via `scip-csharp`, and translate
its output into the same JSON contract `src/symbols/scip_parse.rs` already
defines.

## Step 0 (do this before writing any Rust code): protocol spike

This is the single highest-leverage thing to resolve first, because it
determines almost every downstream design decision (process model,
dependency footprint, release packaging). Do **not** commit to a design and
start wiring `src/symbols/haxe.rs` until this is settled — spike it
standalone, outside codesearch, and report back / confirm with the user.

Two candidate backends:

1. **`haxe-language-server` over LSP (stdio, Content-Length-framed JSON-RPC)**
   — well-documented, standard protocol, but requires Node.js at runtime
   (the server runs as `node server.js`) *and* the language server bundle
   itself, which must be built via `npx lix run vshaxe-build -t
   language-server` (pulls in `lix`, which pulls in a pinned Haxe compiler
   version). Two extra runtime/build dependencies beyond what a Haxe project
   already needs.
2. **Raw `haxe --display` protocol directly against the `haxe` compiler
   binary** — no Node, no language-server middle layer, just the compiler
   the user's Haxe project almost certainly already requires to build. But
   it's a lower-level, less-documented, more compiler-version-sensitive
   protocol (position-encoded request strings like
   `file.hx@<byteoffset>@usage`, not a friendly RPC method).

Spike both against a small real Haxe project (a `.hxml` with a couple of
classes calling each other) *outside* codesearch first:
- For option 1: build `haxe-language-server`'s `bin/server.js`, launch it,
  do the LSP handshake (`initialize` with `displayServerConfig`/`build.hxml`
  in `initializationOptions`, `initialized`, `textDocument/didOpen`,
  `textDocument/references`), confirm you get back sane `Location[]` results
  for a symbol used across two files.
- For option 2: invoke `haxe --display <file>@<offset>@usage
  -cp <classpath> ...` (or whatever the current Haxe version's exact flag
  spelling is — check `haxe --help-display` and the installed compiler's
  actual version, this has drifted across Haxe releases) and see what you
  get back.

Whichever is less painful to drive reliably wins. Report the finding before
proceeding — this is exactly the kind of "ambiguous, architecturally
significant" fork the user should confirm, not something to resolve
silently.

The rest of this document is written assuming **option 1
(haxe-language-server / LSP)** was chosen, since it's the better-documented
and more likely path, but flag clearly in your report if you went with
option 2 instead, since several sections below (framing, init handshake)
won't apply verbatim.

## Architecture shape — how this differs from `csharp.rs`, not just a copy

Don't treat `src/symbols/csharp.rs` as a template to mechanically clone.
The Haxe backend is shaped differently in one important way:

**C#'s Roslyn helper has a cheap "dump every definition in the project"
primitive** (`scip-csharp index`), which is why the C# adapter has an
upfront batch-index phase (`rebuild()` populating `scip_symbols`,
`scip_positions`, `scip_simple_names`) followed by lazy/cached
per-symbol `find-refs` calls (see the "Two-phase reference model" doc
comment at the top of `csharp.rs`).

**LSP has no equivalent project-wide dump primitive.** `haxe-language-server`
is built around single-query, position-anchored requests
(`textDocument/references` at a specific line/column), the same way an
editor cursor works. There is no cheap "list every symbol in the project"
call analogous to `scip-csharp index` — the closest LSP primitives are
`textDocument/documentSymbol` (per-file only) and `workspace/symbol` (name
search, not enumeration).

Implication: **the Haxe adapter should be closer to a live query bridge
than a batch indexer.**
- `find_references_by_position(db_path, file, line)` maps directly onto
  `textDocument/references` at that position — no local LMDB index needed
  for the position→symbol lookup step at all, unlike the C# path's
  `scip_positions` table. You may still want a *thin* on-disk cache of
  `(file, line) → LSP references result` to avoid re-querying the language
  server on repeat calls (mirrors the *spirit* of `scip_ref_cache` in
  `csharp.rs`, but there is no separate "index" phase producing it).
- `find_references(db_path, symbol_name)` (name-based, no file/line given)
  has no direct LSP equivalent — you'd need `workspace/symbol` to resolve a
  name to a position first, then call `textDocument/references` on that
  resolved position. `workspace/symbol` fuzzy-matches by name across the
  whole project, so this two-step (`resolve name → position`, then
  `position → references`) replaces the `scip_simple_names` fuzzy-lookup
  table entirely — you likely don't need to build that table for Haxe.
- `rebuild()` therefore does NOT mean "run a batch indexer and populate
  LMDB." It more naturally means "ensure a live `haxe-language-server`
  process is running (and up to date) for this repo." `has_index()` and
  `index_age()` need reinterpreting accordingly — e.g. "index age" could be
  "seconds since the language server session was last (re)started," and
  `has_index()` could be "is there a live/warm process for this db_path."
  Decide and document this reinterpretation explicitly in the doc comments
  on your `HaxeSymbolIndexer` impl — don't leave the next reader guessing
  why `rebuild()` doesn't write to LMDB the way `csharp.rs`'s does.

Still reuse from the existing architecture as-is:
- The `SymbolIndexer` trait itself (`src/symbols/mod.rs:113-168`) — your
  `HaxeSymbolIndexer` implements the same trait, no trait changes needed.
- Registration: one more `Box::new(haxe::HaxeSymbolIndexer::new())` entry in
  `SymbolIndexerRegistry::new()` (`src/symbols/mod.rs:177-183`).
- The output JSON shape in `scip_parse.rs` if you do want to persist
  anything through that path (e.g. for the reference cache) — it's already
  language-agnostic (`{metadata:{version,tool_info},
  documents:[{relative_path, occurrences:[{range,symbol,symbol_roles,kind}]}]}`).
  You can most likely reuse `parse_find_refs_output`/`FindRefsResult`
  (`scip_parse.rs:82-131`) as-is for the references side without any changes
  to that file, since it's already a generic "list of references for one
  symbol" shape.
- `SymbolReference{file,start_line,end_line,kind}` as the public return
  type — unchanged.

## Process lifecycle — the other big departure from `csharp.rs`

`csharp-scip`'s helper invocations (`index`, `find-refs`,
`batch-find-refs`) are all short-lived: spawn, do the work, exit, read the
output file. `haxe-language-server` is explicitly designed as a **long-lived
process** — the whole point of LSP is that the editor keeps one instance
running per project because startup (loading the full type context of the
project) is expensive, similar in cost/shape to Roslyn's workspace-open cost
that motivated C#'s lazy Opt2 model and Phase-3 pre-warm.

This means the Haxe adapter needs new plumbing that doesn't exist yet
anywhere in `src/symbols/`:
- A per-repo (per `db_path`) cache of a live child process handle + its
  stdio pipes (stdin writer, stdout reader thread demuxing
  Content-Length-framed JSON-RPC responses, matched back to requests by
  LSP request `id`). Guard this behind a `Mutex`, similar spirit to
  `CSharpSymbolIndexer::helper_path` (`csharp.rs:216-230`) but holding a
  live process instead of a resolved path — and note the failure-mode
  difference: a dead/crashed child process needs detecting and
  transparently restarting on the next call, not just cached once like a
  path lookup.
- A restart trigger on `.hx` file changes. The existing `.cs` watcher
  debounce (`CSHARP_REBUILD_DEBOUNCE_SECS`, 60s, referenced in
  `.claude/CLAUDE.md`) is the pattern to mirror — on debounce fire, restart
  (or send the LSP equivalent of "these files changed":
  `textDocument/didChange`/`workspace/didChangeWatchedFiles`) rather than
  re-running a batch index.
- Clean shutdown: send LSP `shutdown` + `exit` when a repo is removed
  (`codesearch index rm`) or on process exit — don't leak child processes.
  There's no existing equivalent to copy for this since C#'s helper never
  stays alive between calls; this is new.

## Position/line/column conversion — a specific, easy-to-get-wrong detail

Flag this prominently in code comments and tests, don't just get it right
once and move on:

- **LSP positions are 0-based lines AND 0-based UTF-16 code-unit character
  offsets** (`{line, character}`), and LSP `Range`s are `{start, end}` pairs
  of those.
- **codesearch's existing convention is 1-based lines, no column**
  (`SymbolReference.start_line`/`end_line`; `.claude/CLAUDE.md` explicitly
  notes "Position lookup matches `start_line` only (not `[start_line,
  end_line]` range)").
- Every conversion at the LSP boundary needs `+1`/`-1` line adjustments in
  the right direction, and the character offset is simply dropped (not
  averaged, not rounded — codesearch doesn't track columns at all today,
  so an LSP `Range` spanning one line becomes `start_line == end_line` at
  that line number; a multi-line range keeps its start/end lines, columns
  discarded). Write a small, directly unit-tested conversion function
  (e.g. `lsp_range_to_symbol_range`) rather than inlining this arithmetic
  at each call site — this is exactly the kind of off-by-one that's easy to
  get backwards and hard to notice until someone's `find_impact` result is
  quietly pointing at the wrong line.

## Project discovery: finding the `.hxml`

C#'s `find_solution()` (`csharp.rs:356-366`) looks for a top-level `.sln`.
The Haxe equivalent is finding a build config — conventionally
`build.hxml`, but any `*.hxml` at the repo root is plausible, and Haxe
projects commonly have *multiple* `.hxml` files for different targets
(e.g. a client build and a server build with different classpaths). Unlike
C#'s single `.sln` assumption:
- Write `find_hxml(repo_path) -> Option<PathBuf>` that looks for
  `build.hxml` first, then any single top-level `*.hxml` if there's exactly
  one, and returns `None` (not a guess) if there are multiple candidates
  and no `build.hxml` — surface this ambiguity as an error/hint rather than
  silently picking one, since picking the wrong target's classpath will
  produce subtly wrong (or missing) reference results.
- Consider (but don't feel obligated to build in v1) a
  `CODESEARCH_HAXE_HXML` override analogous to how `--filter-project`
  disambiguates C# incremental rebuilds, for repos where auto-discovery
  can't disambiguate.
- `applies_to(repo_path)` (mirrors `csharp.rs:1635-1637`'s `.sln` gate)
  should be `find_hxml(repo_path).is_some()`.

## Helper detection & bundling — the biggest new operational cost

This is the part most likely to need a user decision before you can finish,
not just an implementation detail:

- **Build-time**: producing `bin/server.js` requires `npx lix run
  vshaxe-build -t language-server`, which pulls in `lix` → Node/npm → a
  pinned Haxe compiler version. This is an entirely new build toolchain for
  a Rust-only repo — it cannot go through `cargo build`. Model it the way
  `copy-to-common.ps1`'s `dotnet publish` step for `scip-csharp` works: a
  separate, documented build step (e.g. `helpers/haxe/build.sh`) that
  produces the artifact to be staged alongside the codesearch binary, not
  something `cargo` orchestrates.
- **Run-time**: unlike `scip-csharp` (a self-contained `dotnet publish`
  binary with no separate .NET install required on the target machine),
  `bin/server.js` needs a `node` executable on the target machine at run
  time — there's no exact equivalent of self-contained publish for a plain
  Node script. Before committing to shipping it this way, it's worth a
  quick spike into Node's newer single-executable-application packaging
  (`node --experimental-sea-config`) or a bundler like `pkg`/`nexe` to see
  if a genuinely standalone binary (no separate Node install needed) is
  realistic — that would make the detection/bundling story symmetric with
  the C# helper. If that spike doesn't pan out cleanly, requiring Node on
  PATH as a documented prerequisite (like requiring `git`) is an acceptable
  fallback — just be upfront about it rather than discovering it late.
- Either way, mirror the existing detection-order pattern
  (`CSharpSymbolIndexer::resolve_helper_path`, `csharp.rs:254-321`): an env
  var override (e.g. `CODESEARCH_HAXE_LS`) → `<codesearch-exe-dir>/helpers/haxe/server.js`
  → `$PATH`. Also mirror the found/not-found caching
  (`Mutex<Option<Option<PathBuf>>>`) and the filename-validation guard
  against command injection via a malicious env var/PATH entry
  (`validate_helper_path`, `csharp.rs:328-353`) — same threat model
  applies here.
- **Release packaging**: don't assume this should become its own
  independent `-with-haxe` release-variant axis alongside the existing
  `-with-csharp` one — that combinatorially explodes (3 plain × with-csharp ×
  with-haxe = up to 12 archives). Raise this explicitly with the user
  before touching CI; a single combined `-with-helpers` tier (bundling both
  C# and Haxe helpers together) is probably preferable to a full
  cross-product, but that's a call for whoever owns the release matrix, not
  something to decide unilaterally in this doc.

## Constants & registration checklist

Mirror the existing C# constants in `src/constants.rs`
(`SCIP_CSHARP_HELPER_ENV`, `SCIP_CSHARP_HELPER_NAME`,
`SCIP_CSHARP_DEBOUNCE_MS`) with Haxe equivalents. Register the new adapter
in exactly the one place C#'s is registered
(`SymbolIndexerRegistry::new()`, `src/symbols/mod.rs:177-183`) — do not
duplicate instantiation anywhere else; `.claude/CLAUDE.md` is explicit that
there must be exactly 4 `Arc::new(SymbolIndexerRegistry::new())` call sites
total in the whole codebase (`IndexManager::new()`,
`IndexManager::new_for_path()`, `ServeState::new()`,
`CodesearchService::new_with_stores()`) and everything else clones from
those — adding a language never changes that count, it only adds one line
inside the existing `SymbolIndexerRegistry::new()` body.

## Testing

Mirror the gated-integration-test pattern used for C#
(`csharp_helper_integration` cargo feature, `tests/symbols_csharp_test.rs`)
with a `haxe_helper_integration` feature and `tests/symbols_haxe_test.rs`:
a small fixture Haxe project (2-3 classes, one method called from another
class) with a known expected reference location, gated behind the feature
flag so default `cargo test --lib --bins` doesn't require Node/Haxe/the
language server to be installed on every CI runner. Add non-gated unit
tests for the pure-Rust pieces that don't need a live process: the LSP
JSON-RPC framing/parsing helpers, the position-conversion function, and
`find_hxml()`'s discovery logic against fixture directory trees (no
subprocess needed for that last one).

## Validation gates (per `AGENTS.md`)

Same as Phase 1: `cargo check`, `cargo clippy -D warnings`,
`cargo test --lib --bins` (the gated integration test is opt-in, not part
of this default run), no `--release` builds until the end. Do not modify
`src/chunker/` in this task — that's Phase 1's territory and should already
be done and merged before this starts.

## Suggested check-in points

1. After Step 0's spike — confirm the LSP-vs-raw-display-protocol choice
   before writing `src/symbols/haxe.rs`.
2. After the process-lifecycle design (long-lived child process cache,
   restart-on-file-change) is sketched but before full implementation —
   this is the most architecturally novel piece relative to anything
   already in `src/symbols/`, worth a sanity check before investing in it.
3. Before touching CI/release packaging — the `-with-haxe` vs
   `-with-helpers` archive-variant question needs a user decision, not an
   agent guess.
