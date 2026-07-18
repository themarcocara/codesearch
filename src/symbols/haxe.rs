//! Haxe symbol indexer adapter.
//!
//! ## Design decision (post-spike)
//!
//! `HAXE_FIND_IMPACT_TIER_A_HANDOFF.md` originally framed this as a choice
//! between driving `haxe-language-server` over LSP, or the Haxe compiler's
//! raw `--display` protocol directly, and asked for a spike before writing
//! any Rust code. That spike found that `haxe <hxml> --display
//! <file>@<byteOffset>@usage` — a plain synchronous CLI invocation of the
//! vanilla `haxe` compiler, no language server, no Node.js — already
//! returns real, type-resolved cross-file reference locations (verified
//! against a small multi-file test project: correctly found both call
//! sites of a static method from either the definition or a call site,
//! and ignored an unrelated same-named local). That makes the Haxe
//! compiler itself the "helper" — there is nothing to build or bundle,
//! unlike `scip-csharp` which had to wrap Roslyn in a purpose-built CLI.
//! This adapter shells out to the user's own `haxe` install (detected like
//! any other language toolchain), using `tree-sitter-haxe` (already a
//! codesearch dependency for Phase 1 chunking) purely to resolve a
//! `(file, line)` position or a symbol name to a precise byte offset, then
//! asks the compiler for the semantic reference list at that offset.
//!
//! ## Why there's no batch index (`scip_symbols`/`scip_positions`/...)
//!
//! Unlike Roslyn (wrapped by `scip-csharp`), Haxe's `--display` protocol has
//! no "dump every symbol in the project" primitive — it's a single-query,
//! position-anchored protocol, the same way an editor's "Find References"
//! click is. So `rebuild()` doesn't populate an LMDB symbol table; it just
//! verifies the helper + `.hxml` are present and records `repo_path` plus
//! a last-verified timestamp in a small `haxe_meta` table, so
//! `find_references`/`find_references_by_position` know where to find the
//! `.hxml` and can answer `has_index`/`index_age`.
//!
//! ## Portable Haxe SDKs and `HAXE_STD_PATH` (e.g. Kode/Kha)
//!
//! Some Haxe distributions are a self-contained directory (compiler binary
//! + `std/` standard library, sometimes shipped alongside per-platform
//! runtime DLLs on Windows) rather than a system install — notably the
//! `Kode/kha` game framework, which vendors its own per-platform Haxe SDK
//! under `Tools/<platform>/` rather than trusting whatever `haxe` a
//! developer might have on `$PATH`. Investigated directly: that binary
//! auto-discovers a *sibling* `std/` next to itself when present, but fails
//! with "Standard library not found" once separated from it — and Kha's own
//! build tool (`khamake`) never relies on that auto-discovery, always
//! setting `HAXE_STD_PATH` explicitly instead. `invoke_display_usage`
//! mirrors that: when the resolved `haxe` binary has a sibling `std/`
//! directory, `HAXE_STD_PATH` is set to it for that invocation,
//! unconditionally overriding whatever the ambient environment has — a std/
//! library that doesn't match the resolved binary's version is a
//! version-mismatch footgun, not something to defer to just because it
//! happened to already be set. See `sibling_std_dir`.
//!
//! ## Known limitation: cold invocation per query (no compile-server yet)
//!
//! Each query spawns a fresh `haxe <hxml> --display ...` process, which
//! recompiles the project's type graph from scratch (~0.2s in testing on a
//! tiny two-file project; slower on large real-world projects). Haxe ships
//! a built-in compilation-cache server (`haxe --wait <port>`, then `haxe
//! --connect <port> ...` for subsequent queries — measured ~10x faster
//! once warm in testing) that would remove this cost across repeat calls.
//! Managing a long-lived per-repo `--wait` process (spawn-once,
//! restart-on-file-change, clean shutdown) is a real chunk of additional
//! plumbing that improves latency, not correctness, so it's deliberately
//! left as a scoped follow-up rather than folded into this first cut.
//!
//! ## Known gap: no `workspace/symbol`-style name search
//!
//! `find_references` (given a symbol *name*, no file/line) has no direct
//! compiler primitive to resolve a name to a position the way LSP's
//! `workspace/symbol` would. `find_definition_by_name` below stands in for
//! it by scanning `.hx` files with `tree-sitter-haxe` for a matching
//! declaration name — syntactic, not semantic, so it can pick the wrong
//! same-named declaration across unrelated types. Once a candidate
//! position is found, though, the actual reference resolution from that
//! position is fully semantic via `--display @usage`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use heed::types::Str;
use heed::{Database, EnvOpenOptions};

use crate::constants::{
    HAXE_HELPER_ENV, HAXE_HELPER_NAME, HAXE_META_DB_NAME, HAXE_META_REBUILD_TIMESTAMP_KEY,
    HAXE_META_REPO_PATH_KEY,
};
use crate::lmdb_registry::TrackedEnv;

use super::{RebuildScope, RebuildSummary, SymbolIndexer, SymbolReference};

/// Node kinds recognized as definitions, mirrored from
/// `HaxeExtractor::definition_types()` in `src/chunker/extractor.rs`.
/// Kept as an independent copy rather than a shared dependency so
/// `src/symbols` and `src/chunker` stay decoupled (same reasoning as the
/// rest of this module boundary) — if the grammar's node kinds change,
/// update both lists together.
const DEFINITION_KINDS: &[&str] = &[
    "class_declaration",
    "interface_declaration",
    "enum_declaration",
    "enum_abstract_declaration",
    "typedef_declaration",
    "function_declaration",
];

pub struct HaxeSymbolIndexer {
    /// Cached detection result for the `haxe` binary.
    /// `None` = not yet attempted. `Some(None)` = attempted, not found.
    /// `Some(Some(path))` = found at given path.
    helper_path: Mutex<Option<Option<PathBuf>>>,
}

impl Default for HaxeSymbolIndexer {
    fn default() -> Self {
        Self::new()
    }
}

impl HaxeSymbolIndexer {
    pub fn new() -> Self {
        Self {
            helper_path: Mutex::new(None),
        }
    }

    /// Locate the `haxe` compiler binary.
    ///
    /// Search order:
    /// 1. `CODESEARCH_HAXE` env var
    /// 2. `$PATH` lookup
    ///
    /// Unlike `scip-csharp`, this is never bundled next to the codesearch
    /// binary — it's the user's own Haxe SDK install, the same one their
    /// project already needs to build.
    pub fn detect_helper(&self) -> Option<PathBuf> {
        {
            let lock = self.helper_path.lock().unwrap();
            if let Some(cached) = lock.as_ref() {
                return cached.clone();
            }
        }
        let resolved = self.resolve_helper_path();
        let mut lock = self.helper_path.lock().unwrap();
        *lock = Some(resolved.clone());
        resolved
    }

    fn resolve_helper_path(&self) -> Option<PathBuf> {
        if let Ok(path) = std::env::var(HAXE_HELPER_ENV) {
            let p = PathBuf::from(&path);
            if p.is_file() {
                return Some(p);
            }
            tracing::warn!(
                "{}={} does not exist, falling back to PATH",
                HAXE_HELPER_ENV,
                path
            );
        }

        let lookup_cmd = if cfg!(windows) { "where" } else { "which" };
        if let Ok(output) = Command::new(lookup_cmd).arg(HAXE_HELPER_NAME).output() {
            if output.status.success() {
                let path_str = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let p = PathBuf::from(&path_str);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
        None
    }

    /// Find the project's `.hxml` build file.
    ///
    /// Prefers `build.hxml`; falls back to a single top-level `*.hxml` if
    /// there's exactly one. Returns `None` (not a guess) when there are
    /// multiple candidates and no `build.hxml` — picking the wrong
    /// target's classpath would produce subtly wrong or missing references.
    fn find_hxml(repo_path: &Path) -> Option<PathBuf> {
        let build_hxml = repo_path.join("build.hxml");
        if build_hxml.is_file() {
            return Some(build_hxml);
        }
        let mut candidates = Vec::new();
        if let Ok(entries) = std::fs::read_dir(repo_path) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("hxml") {
                    candidates.push(p);
                }
            }
        }
        if candidates.len() == 1 {
            candidates.pop()
        } else {
            None
        }
    }

    /// Open (creating if needed) the tiny metadata-only LMDB env for this
    /// adapter. Not a symbol index — just enough state to answer
    /// `has_index`/`index_age` and resolve `repo_path` from `db_path`.
    fn open_meta_env(&self, db_path: &Path) -> Result<TrackedEnv> {
        let haxe_dir = db_path.join("haxe");
        std::fs::create_dir_all(&haxe_dir).with_context(|| {
            format!(
                "Failed to create Haxe meta directory: {}",
                haxe_dir.display()
            )
        })?;
        let mut opts = EnvOpenOptions::new();
        // Metadata only (two small string keys) — nowhere near LMDB's default.
        opts.map_size(8 * 1024 * 1024).max_dbs(2);
        let env =
            unsafe { TrackedEnv::open(&opts, &haxe_dir, &format!("HAXE({})", db_path.display()))? };
        let mut wtxn = env.write_txn()?;
        env.create_database::<Str, Str>(&mut wtxn, Some(HAXE_META_DB_NAME))?;
        wtxn.commit()?;
        Ok(env)
    }

    fn read_repo_path(&self, db_path: &Path) -> Result<Option<PathBuf>> {
        let env = self.open_meta_env(db_path)?;
        let rtxn = env.read_txn()?;
        let meta_db: Database<Str, Str> = env
            .open_database(&rtxn, Some(HAXE_META_DB_NAME))?
            .ok_or_else(|| anyhow::anyhow!("haxe_meta database not found"))?;
        Ok(meta_db
            .get(&rtxn, HAXE_META_REPO_PATH_KEY)?
            .map(PathBuf::from))
    }

    /// Resolve a 1-based line number to the byte offset of a recognized
    /// declaration's name starting on that line, via `tree-sitter-haxe`.
    fn resolve_position_to_offset(source: &[u8], line: u32) -> Option<usize> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_haxe::LANGUAGE.into())
            .ok()?;
        let tree = parser.parse(source, None)?;
        let target_row = line.checked_sub(1)? as usize;
        find_name_offset_at_row(tree.root_node(), target_row)
    }

    /// Scan every `.hx` file under `repo_path` for a definition whose
    /// extracted name matches `symbol` (last dotted/`#`-segment, exact
    /// match preferred; falls back to the first substring match), returning
    /// its file (relative to `repo_path`) and the name's byte offset.
    fn find_definition_by_name(repo_path: &Path, symbol: &str) -> Option<(PathBuf, usize)> {
        let simple = symbol.rsplit(['.', '#']).next().unwrap_or(symbol);
        let mut fallback: Option<(PathBuf, usize)> = None;

        for path in walk_hx_files(repo_path) {
            let Ok(source) = std::fs::read(&path) else {
                continue;
            };
            let mut parser = tree_sitter::Parser::new();
            if parser
                .set_language(&tree_sitter_haxe::LANGUAGE.into())
                .is_err()
            {
                continue;
            }
            let Some(tree) = parser.parse(&source, None) else {
                continue;
            };
            let mut defs = Vec::new();
            collect_definitions(tree.root_node(), &source, &mut defs);
            let rel = path.strip_prefix(repo_path).unwrap_or(&path).to_path_buf();
            for (offset, name) in defs {
                if name == simple {
                    return Some((rel, offset));
                }
                if fallback.is_none() && name.contains(simple) {
                    fallback = Some((rel.clone(), offset));
                }
            }
        }
        fallback
    }

    /// Invoke `haxe <hxml> --display <file>@<offset>@usage` and parse the
    /// result. `file` is relative to `repo_path`. The definition site
    /// itself is never included in `--display @usage` output (confirmed by
    /// testing) — every entry returned here is a genuine reference/call
    /// site, so all are tagged `kind = "reference"`.
    ///
    /// The compiler writes `--display` output to **stderr**, not stdout
    /// (confirmed by testing — this is easy to get backwards, since a
    /// terminal shows both streams interleaved and looks fine either way;
    /// reading stdout alone silently returns nothing). Both streams are
    /// concatenated defensively and fed to the same line-based parser,
    /// which only recognizes `<pos>...</pos>` lines and ignores anything
    /// else, so this is safe even if a different Haxe version/config ever
    /// routes it to stdout instead.
    fn invoke_display_usage(
        &self,
        haxe_bin: &Path,
        hxml: &Path,
        repo_path: &Path,
        file: &Path,
        offset: usize,
    ) -> Result<Vec<SymbolReference>> {
        let display_arg = format!("{}@{}@usage", file.display(), offset);
        let mut cmd = Command::new(haxe_bin);
        cmd.current_dir(repo_path)
            .arg(hxml)
            .arg("--display")
            .arg(&display_arg);

        // Portable Haxe distributions (e.g. Kode/Kha's vendored `Tools/<platform>/`
        // SDKs) ship a `std/` directory as a sibling of the compiler binary and
        // rely on the caller setting HAXE_STD_PATH to it -- confirmed by testing:
        // the binary auto-discovers a *sibling* std/ next to itself when present,
        // but errors with "Standard library not found" when separated from it,
        // and khamake (Kha's own build tool) never relies on that auto-discovery,
        // always setting HAXE_STD_PATH explicitly instead. We do the same, and
        // deliberately override rather than defer to any ambient HAXE_STD_PATH:
        // once a specific haxe binary has been resolved (env var or PATH), a std/
        // sitting right next to it is the one guaranteed to match that binary's
        // version -- a stray unrelated HAXE_STD_PATH left over from some other
        // Haxe install is a version-mismatch footgun, not a preference to respect.
        if let Some(std_dir) = sibling_std_dir(haxe_bin) {
            cmd.env("HAXE_STD_PATH", std_dir);
        }

        let output = cmd
            .output()
            .with_context(|| format!("Failed to execute haxe at {}", haxe_bin.display()))?;

        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
        let refs = parse_usage_output(&combined)?;

        // The compiler reports files using its own module-resolution paths,
        // which are not guaranteed relative to repo_path (e.g. a referencing
        // file reached only via classpath resolution, not the queried file
        // itself) — normalize to match SymbolReference's documented
        // "relative to project root" convention.
        Ok(refs
            .into_iter()
            .map(|mut r| {
                if let Ok(rel) = r.file.strip_prefix(repo_path) {
                    r.file = rel.to_path_buf();
                }
                r
            })
            .collect())
    }
}

impl SymbolIndexer for HaxeSymbolIndexer {
    fn language(&self) -> &str {
        "haxe"
    }

    fn rebuild(
        &self,
        repo_path: &Path,
        db_path: &Path,
        _scope: RebuildScope,
    ) -> Result<RebuildSummary> {
        let start = std::time::Instant::now();

        self.detect_helper().ok_or_else(|| {
            anyhow::anyhow!(
                "haxe compiler not found on PATH. Install the Haxe SDK or set {} \
                 to the haxe binary path.",
                HAXE_HELPER_ENV
            )
        })?;
        Self::find_hxml(repo_path).ok_or_else(|| {
            anyhow::anyhow!(
                "No build.hxml (or single top-level *.hxml) found in {}",
                repo_path.display()
            )
        })?;

        // No batch index to populate (see module docs) — just record enough
        // to resolve repo_path from db_path on later queries.
        let env = self.open_meta_env(db_path)?;
        let mut wtxn = env.write_txn()?;
        let meta_db: Database<Str, Str> =
            env.create_database(&mut wtxn, Some(HAXE_META_DB_NAME))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        meta_db.put(
            &mut wtxn,
            HAXE_META_REPO_PATH_KEY,
            repo_path.to_string_lossy().as_ref(),
        )?;
        meta_db.put(
            &mut wtxn,
            HAXE_META_REBUILD_TIMESTAMP_KEY,
            now.to_string().as_str(),
        )?;
        wtxn.commit()?;

        Ok(RebuildSummary {
            symbols_indexed: 0,
            references_stored: 0,
            duration_ms: start.elapsed().as_millis() as u64,
        })
    }

    fn find_references(&self, db_path: &Path, symbol: &str) -> Result<Vec<SymbolReference>> {
        let repo_path = self.read_repo_path(db_path)?.ok_or_else(|| {
            anyhow::anyhow!("No Haxe index metadata for this project. Run a rebuild first.")
        })?;
        let haxe_bin = self.detect_helper().ok_or_else(|| {
            anyhow::anyhow!(
                "haxe compiler not found on PATH. Set {} to override.",
                HAXE_HELPER_ENV
            )
        })?;
        let hxml = Self::find_hxml(&repo_path).ok_or_else(|| {
            anyhow::anyhow!("No .hxml build file found in {}", repo_path.display())
        })?;

        let Some((file, offset)) = Self::find_definition_by_name(&repo_path, symbol) else {
            tracing::debug!("No Haxe declaration matching '{}' found", symbol);
            return Ok(vec![]);
        };

        self.invoke_display_usage(&haxe_bin, &hxml, &repo_path, &file, offset)
    }

    fn find_references_by_position(
        &self,
        db_path: &Path,
        file: &Path,
        line: u32,
    ) -> Result<Vec<SymbolReference>> {
        let repo_path = self.read_repo_path(db_path)?.ok_or_else(|| {
            anyhow::anyhow!("No Haxe index metadata for this project. Run a rebuild first.")
        })?;
        let haxe_bin = self.detect_helper().ok_or_else(|| {
            anyhow::anyhow!(
                "haxe compiler not found on PATH. Set {} to override.",
                HAXE_HELPER_ENV
            )
        })?;
        let hxml = Self::find_hxml(&repo_path).ok_or_else(|| {
            anyhow::anyhow!("No .hxml build file found in {}", repo_path.display())
        })?;

        let abs_file = repo_path.join(file);
        let source = std::fs::read(&abs_file)
            .with_context(|| format!("Failed to read {}", abs_file.display()))?;

        let Some(offset) = Self::resolve_position_to_offset(&source, line) else {
            tracing::debug!("No Haxe declaration found at {}:{}", file.display(), line);
            return Ok(vec![]);
        };

        self.invoke_display_usage(&haxe_bin, &hxml, &repo_path, file, offset)
    }

    fn index_age(&self, db_path: &Path) -> u64 {
        let Ok(env) = self.open_meta_env(db_path) else {
            return u64::MAX;
        };
        let Ok(rtxn) = env.read_txn() else {
            return u64::MAX;
        };
        let Ok(Some(meta_db)) = env.open_database::<Str, Str>(&rtxn, Some(HAXE_META_DB_NAME))
        else {
            return u64::MAX;
        };
        let Ok(Some(ts_str)) = meta_db.get(&rtxn, HAXE_META_REBUILD_TIMESTAMP_KEY) else {
            return u64::MAX;
        };
        let Ok(stored_ts) = ts_str.parse::<u64>() else {
            return u64::MAX;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now.saturating_sub(stored_ts)
    }

    fn has_index(&self, db_path: &Path) -> bool {
        db_path.join("haxe").exists() && self.index_age(db_path) != u64::MAX
    }

    fn is_available(&self) -> bool {
        self.detect_helper().is_some()
    }

    fn applies_to(&self, repo_path: &Path) -> bool {
        Self::find_hxml(repo_path).is_some()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Depth-first search for a definition node (one of `DEFINITION_KINDS`)
/// whose `name` field starts on `target_row` (0-based, tree-sitter
/// convention). Returns the name's byte offset.
fn find_name_offset_at_row(node: tree_sitter::Node, target_row: usize) -> Option<usize> {
    if DEFINITION_KINDS.contains(&node.kind()) {
        if let Some(name) = node.child_by_field_name("name") {
            if name.start_position().row == target_row {
                return Some(name.start_byte());
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if let Some(found) = find_name_offset_at_row(child, target_row) {
            return Some(found);
        }
    }
    None
}

/// Depth-first collection of every definition node's `(name_byte_offset,
/// name_text)`, in document order.
fn collect_definitions(node: tree_sitter::Node, source: &[u8], out: &mut Vec<(usize, String)>) {
    if DEFINITION_KINDS.contains(&node.kind()) {
        if let Some(name) = node.child_by_field_name("name") {
            if let Ok(text) = name.utf8_text(source) {
                out.push((name.start_byte(), text.to_string()));
            }
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_definitions(child, source, out);
    }
}

/// Recursively list every `.hx` file under `repo_path` (skips hidden dirs
/// like `.git`).
fn walk_hx_files(repo_path: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_hx_files_inner(repo_path, &mut out);
    out
}

fn walk_hx_files_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            walk_hx_files_inner(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("hx") {
            out.push(path);
        }
    }
}

/// If `haxe_bin`'s directory contains a `std/` subdirectory, return its
/// path. Mirrors `khamake`'s `Haxe.ts`: `path.resolve(haxeDirectory, 'std')`,
/// used to derive `HAXE_STD_PATH` for portable/vendored Haxe SDKs (e.g.
/// Kode/Kha's `Tools/<platform>/` distributions) where the compiler and its
/// standard library travel together as a directory, not a system install.
fn sibling_std_dir(haxe_bin: &Path) -> Option<PathBuf> {
    let dir = haxe_bin.parent()?;
    let std_dir = dir.join("std");
    if std_dir.is_dir() {
        Some(std_dir)
    } else {
        None
    }
}

/// Parse `haxe --display <file>@<offset>@usage` stdout into references.
///
/// Expected format, one entry per reference:
/// `<pos>path/to/File.hx:LINE: characters START-END</pos>`
/// (wrapped in a top-level `<list>...</list>`, and empty when there are no
/// references). Character columns are parsed only to validate the format;
/// codesearch's `SymbolReference` doesn't track columns (matches the
/// existing SCIP position convention — see `.claude/CLAUDE.md`), so only
/// the file and line are kept.
fn parse_usage_output(stdout: &str) -> Result<Vec<SymbolReference>> {
    let mut refs = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        let Some(inner) = line
            .strip_prefix("<pos>")
            .and_then(|s| s.strip_suffix("</pos>"))
        else {
            continue;
        };
        let Some((file_and_line, _characters)) = inner.split_once(": characters ") else {
            tracing::debug!("Unrecognized haxe --display usage entry: {}", line);
            continue;
        };
        let Some((path_str, line_str)) = file_and_line.rsplit_once(':') else {
            tracing::debug!("Unrecognized haxe --display usage entry: {}", line);
            continue;
        };
        match line_str.trim().parse::<u32>() {
            Ok(line_num) => refs.push(SymbolReference {
                file: PathBuf::from(path_str),
                start_line: line_num,
                end_line: line_num,
                kind: "reference".to_string(),
            }),
            Err(_) => tracing::debug!("Unrecognized haxe --display usage entry: {}", line),
        }
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_usage_output_basic() {
        let stdout = "<list>\n<pos>src/Main.hx:3: characters 21-29</pos>\n\
                      <pos>src/Main.hx:4: characters 22-30</pos>\n</list>\n";
        let refs = parse_usage_output(stdout).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].file, PathBuf::from("src/Main.hx"));
        assert_eq!(refs[0].start_line, 3);
        assert_eq!(refs[0].end_line, 3);
        assert_eq!(refs[0].kind, "reference");
        assert_eq!(refs[1].start_line, 4);
    }

    #[test]
    fn test_parse_usage_output_empty() {
        let refs = parse_usage_output("<list>\n</list>\n").unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn test_parse_usage_output_ignores_garbage() {
        let refs = parse_usage_output("Error: something went wrong\n").unwrap();
        assert!(refs.is_empty());
    }

    #[test]
    fn test_find_hxml_prefers_build_hxml() {
        let dir = std::env::temp_dir().join(format!(
            "hx_test_hxml_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("build.hxml"), "-cp src\n").unwrap();
        std::fs::write(dir.join("other.hxml"), "-cp src2\n").unwrap();

        let found = HaxeSymbolIndexer::find_hxml(&dir);
        assert_eq!(found, Some(dir.join("build.hxml")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_find_hxml_ambiguous_returns_none() {
        let dir = std::env::temp_dir().join(format!(
            "hx_test_hxml_ambig_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("client.hxml"), "-cp src\n").unwrap();
        std::fs::write(dir.join("server.hxml"), "-cp src2\n").unwrap();

        assert_eq!(HaxeSymbolIndexer::find_hxml(&dir), None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_sibling_std_dir_present() {
        let dir = std::env::temp_dir().join(format!(
            "hx_test_std_present_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("std")).unwrap();
        let haxe_bin = dir.join("haxe");
        std::fs::write(&haxe_bin, "").unwrap();

        assert_eq!(sibling_std_dir(&haxe_bin), Some(dir.join("std")));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_sibling_std_dir_absent() {
        let dir = std::env::temp_dir().join(format!(
            "hx_test_std_absent_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let haxe_bin = dir.join("haxe");
        std::fs::write(&haxe_bin, "").unwrap();

        // No "std" subdirectory next to the binary -- e.g. a system-installed
        // `haxe` on $PATH, which resolves its std library some other way.
        assert_eq!(sibling_std_dir(&haxe_bin), None);

        std::fs::remove_dir_all(&dir).ok();
    }
}
