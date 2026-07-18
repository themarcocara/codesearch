# Handoff: Haxe language support for codesearch

Written for whichever agent picks this up next. You have no memory of the
conversation that produced this document — everything you need is below or
linked. Read `AGENTS.md` and `.claude/CLAUDE.md` at the repo root first; they
are the source of truth for this repo's architecture, branching workflow, and
validation gates, and this document does not repeat their contents except
where directly relevant to Haxe.

## Grammar source

The user has forked `vantreeseba/tree-sitter-haxe` to
`themarcocara/tree-sitter-haxe`. Use that fork as the grammar dependency, not
the upstream repo. As of this writing the fork is an unmodified copy of
upstream — see "Known issue to fix first" below before you can build against
it.

## Two separable features — do not conflate them

codesearch has two independent places where "language support" means
something different. This handoff is scoped to **Phase 1 only**. Do not start
Phase 2 without explicit user sign-off — it's a much bigger, different kind of
task and the user has not asked for it yet.

### Phase 1 (this task): semantic chunking for embedding search

`src/chunker/` uses tree-sitter grammars to find function/class/etc.
boundaries so embedded search chunks align with code structure instead of
naive line splits. 13 languages are wired up today (Rust, Python, JS/TS, C,
C++, C#, Go, Java, Bash, Ruby, PHP, YAML, JSON, Markdown, Dart — the last of
which is the best template to copy, see below). This is a purely syntactic,
in-process, low-risk addition. **This is the task.**

### Phase 2 (explicitly out of scope — do not build this unless asked)

`src/symbols/` powers the `find_impact` MCP tool — file/line-precise
"find all references" for refactor planning. The C# implementation
(`src/symbols/csharp.rs`) shells out to a bundled Roslyn-based helper
(`scip-csharp`) that does real semantic resolution via
`SymbolFinder.FindReferencesAsync()` — types, overloads, cross-file imports,
the works. A tree-sitter grammar cannot provide that: it has no type system
and no cross-file resolution, only a syntax tree. A Haxe equivalent of
`find_impact` with comparable accuracy would need a helper built on the Haxe
compiler's own `--display` / JSON-RPC completion protocol (what
`haxe-language-server`/vshaxe use), analogous in spirit to `scip-csharp` but
a substantially different and heavier build. If asked to do this later, note
that `src/symbols/scip_parse.rs`'s JSON schema
(`{metadata:{version,tool_info}, documents:[{relative_path,
occurrences:[{range,symbol,symbol_roles,kind}]}], external_symbols:[]}`) is
already language-agnostic — a Haxe helper only needs to emit that same shape
and register a `HaxeSymbolIndexer` in `SymbolIndexerRegistry::new()`
(`src/symbols/mod.rs`). But again: **not this task.**

## Known issue to fix first: grammar/bindings version mismatch

`themarcocara/tree-sitter-haxe`'s `Cargo.toml` pins `tree-sitter ~0.20.3` and
its `bindings/rust/lib.rs` uses the old tree-sitter-cli binding style
(`extern "C" fn tree_sitter_haxe() -> Language`). This repo's `Cargo.toml`
pins `tree-sitter = "0.26.8"` and every existing grammar dependency
(`tree-sitter-rust`, `tree-sitter-dart`, etc.) uses the modern `LANGUAGE`
const style (`tree_sitter_dart::LANGUAGE.into()` — see
`src/chunker/grammar.rs:61-80`). Pulling in the fork as-is will either fail
to compile against 0.26.8 or drag a second incompatible `tree-sitter` crate
version into the dependency graph.

Before wiring up the Rust side, in `themarcocara/tree-sitter-haxe`:
1. Regenerate `bindings/rust/` (and `bindings/rust/build.rs`) with a current
   `tree-sitter-cli` (matching what generated the other grammars this repo
   depends on) so it exports the `LANGUAGE` const, not the old `extern "C"`
   function.
2. Bump the `Cargo.toml` `tree-sitter` dependency to match (or at least be
   ABI-compatible with) `0.26.x`.
3. Verify `grammar.js` still parses cleanly with the regenerated bindings —
   run the grammar's own test corpus if present.
4. Tag a commit (or release) once this is done, and pin the codesearch-side
   `Cargo.toml` dependency to that exact git revision — not a floating
   branch — so a future upstream force-push or branch change can't silently
   break this repo's build.

If regenerating the bindings turns out to be more involved than expected
(e.g. the grammar itself doesn't compile cleanly under a current
tree-sitter-cli), stop and report back rather than papering over parser
errors — a broken/incomplete Haxe grammar will produce silently wrong chunk
boundaries, which is worse than no Haxe support.

## Phase 1 implementation plan

Use the Dart language addition as your template — it is the most recent
"add one more tree-sitter grammar" change in this repo's history and touches
exactly the right surface. Look at it directly:

```
git show 0624012   # "feat: add Dart language support with tree-sitter semantic chunking"
```

That commit touched exactly these 5 files, and your Haxe change should touch
the equivalent five (plus `Cargo.lock`, updated automatically by `cargo
build`):

1. **`Cargo.toml`** — add the grammar dependency:
   ```toml
   tree-sitter-haxe = { git = "https://github.com/themarcocara/tree-sitter-haxe", rev = "<pinned-sha>" }
   ```
   (Use `rev`, not `branch`/`tag`, once you have the fixed-bindings commit
   SHA from the step above.) Also bump the `[package] version` patch number
   per this repo's convention (a pre-commit hook may do this automatically —
   check before hand-editing).

2. **`src/file/language.rs`**:
   - Add a `Haxe` variant to the `Language` enum (next to `Dart` is a
     reasonable spot, matching Dart's own placement next to `Kotlin`).
   - Add extension mapping in `from_extension`: `"hx" => Self::Haxe,`. Also
     consider whether `.hxml` (Haxe build files) should map here or stay
     `Unknown` — `.hxml` is a build-config DSL, not Haxe source, so it
     should probably NOT map to `Language::Haxe`. Leave it unmapped unless
     you have a specific reason to chunk it.
   - Add `Self::Haxe` to whichever match arm gates "supports tree-sitter
     parsing" (the `Self::Kotlin | Self::Dart | ...` block Dart was added
     to at `src/file/language.rs:102-107` in the reference commit).
   - Add `Self::Haxe => "Haxe",` to the `name()` method.

3. **`src/chunker/grammar.rs`**:
   - Add `Language::Haxe => Ok(tree_sitter_haxe::LANGUAGE.into()),` in
     `load_grammar` (mirrors `src/chunker/grammar.rs:61-80`).
   - Add `Language::Haxe` to the `supported_languages()` vec
     (`src/chunker/grammar.rs:89-...`).

4. **`src/chunker/extractor.rs`** — this is the real work:
   - Add `Language::Haxe => Some(Box::new(HaxeExtractor)),` to
     `get_extractor` (`src/chunker/extractor.rs:81-94`).
   - Implement `HaxeExtractor: LanguageExtractor` (trait defined at
     `src/chunker/extractor.rs:16-78`; `DartExtractor` at
     `src/chunker/extractor.rs:982-1126` is your closest template — Dart
     and Haxe are both C-family, class-based, statically-typed OO
     languages with similar declaration shapes).
   - You will need the *actual* node kind names from
     `themarcocara/tree-sitter-haxe`'s `grammar.js` / generated
     `node-types.json` — do not guess these from Dart's. At minimum, Haxe
     has: `class_declaration`-equivalent, `interface_declaration`,
     `enum_declaration` (including Haxe's enum-with-constructors form,
     which is semantically closer to an ADT than Dart/Java enums —
     classify it as `ChunkKind::Enum` regardless), `typedef_declaration`
     (→ `ChunkKind::TypeAlias`), `abstract_declaration` (Haxe's `abstract`
     types have no Dart/Java equivalent — decide a `ChunkKind` for it,
     `ChunkKind::TypeAlias` or `ChunkKind::Other` are the closest fits),
     function/method declarations, and doc comments (`/** */`, Haxe does
     not use `///`). Read the grammar's own `test/corpus/*.txt` files in
     the tree-sitter-haxe repo — they contain example source alongside the
     exact S-expression node names the grammar produces, which is more
     reliable than reading `grammar.js` rules directly.
   - `ChunkKind` variants are defined in `src/chunker/mod.rs` (or wherever
     `super::ChunkKind` resolves from `extractor.rs`) — check what's
     available before inventing a mapping; reuse existing variants
     (`Class`, `Interface`, `Enum`, `Function`, `Method`, `TypeAlias`,
     `Other`) rather than adding a new one unless Haxe has a construct with
     no reasonable existing fit.

5. No test fixtures directory changes were made for Dart (`tests/fixtures`
   has no dart-specific files), so don't feel obligated to add a
   `tests/fixtures/*.hx` corpus unless you want one — but DO add unit tests
   for `HaxeExtractor` methods directly in `extractor.rs`'s `#[cfg(test)]
   mod tests` block (existing extractors mostly rely on this rather than
   fixture files — check what's there for Rust/Python/Go as a pattern).
   Cover at minimum: name extraction for class/function/method, signature
   extraction, docstring extraction, and `classify()` for each definition
   type in `definition_types()`.

## Validation gates (per `AGENTS.md`)

- `cargo check` while iterating.
- `cargo clippy -D warnings` before considering this done — must be clean,
  no allow-and-move-on.
- `cargo test --lib --bins` — must pass, including your new extractor tests.
- Do **not** run `--release` builds until the very end, if at all, per
  `AGENTS.md`.
- Do not touch `src/symbols/` in this task (see Phase 2 section above).

## Branching / PR workflow (per `AGENTS.md` — read the full section there)

- Integration branch is `develop`, not `master`. Target PRs with
  `--base develop`.
- Merge style is merge commits, not squash, for feature branches.
- If you're continuing on the branch this repo session was already using
  (`claude/haxe-indexer-support-8g382d`), check `git status` / `git log`
  first — this handoff document itself may already be committed there, and
  you should build on top of it rather than starting a fresh branch.

## Suggested check-in points

Given the grammar-fix step (regenerating bindings on the fork) is the
riskiest and most open-ended part, it's reasonable to pause and confirm with
the user once that's done and `themarcocara/tree-sitter-haxe` builds cleanly
against `tree-sitter 0.26.x`, before proceeding to the Rust-side wiring —
especially if the grammar's own corpus reveals gaps or bugs that would affect
chunk quality.
