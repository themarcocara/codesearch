//! LMDB environment tracking to prevent double-open panics.
//!
//! LMDB does not allow two `EnvOpenOptions::open()` handles on the same directory
//! in the same process with different options. Violating this causes runtime panics
//! and corrupted indexes.
//!
//! This module provides [`TrackedEnv`], a thin wrapper around `heed::Env` that
//! registers every open in a global `DashMap` and unregisters on Drop. If a
//! second open is attempted on the same canonical path, it returns a clear error
//! instead of a cryptic LMDB panic.

use anyhow::{Context, Result};
use dashmap::DashMap;
use std::mem::ManuallyDrop;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use crate::cache::safe_canonicalize;

// ── Global registry ─────────────────────────────────────────────

static LMDB_REGISTRY: OnceLock<DashMap<PathBuf, LmdbEntry>> = OnceLock::new();

#[derive(Debug)]
struct LmdbEntry {
    description: String,
    opened_at: Instant,
}

fn register(path: &Path, description: &str) -> Result<PathBuf> {
    let registry = LMDB_REGISTRY.get_or_init(DashMap::new);
    let canonical = safe_canonicalize(path)
        .with_context(|| format!("Cannot canonicalize LMDB path: {}", path.display()))?;

    // Use DashMap's atomic entry API to prevent TOCTOU race between check+insert.
    use dashmap::mapref::entry::Entry;
    match registry.entry(canonical.clone()) {
        Entry::Occupied(existing) => {
            let entry = existing.get();
            anyhow::bail!(
                "LMDB double-open prevented: {} is already open ({}, opened {:.1}s ago)",
                canonical.display(),
                entry.description,
                entry.opened_at.elapsed().as_secs_f64()
            );
        }
        Entry::Vacant(slot) => {
            slot.insert(LmdbEntry {
                description: description.to_string(),
                opened_at: Instant::now(),
            });
        }
    }

    Ok(canonical)
}

fn unregister(canonical: &Path) {
    if let Some(registry) = LMDB_REGISTRY.get() {
        registry.remove(canonical);
    }
}

// ── TrackedEnv wrapper ──────────────────────────────────────────

/// Wrapper around [`heed::Env`] that prevents double-open panics.
///
/// On creation, registers the LMDB path in a global registry. If another
/// `TrackedEnv` is already open on the same canonical path, returns an error
/// with context about who opened it and when. On drop, unregisters automatically.
///
/// Implements `Deref<Target = heed::Env>` so all existing `env.method()` calls
/// work without changes.
pub struct TrackedEnv {
    /// Wrapped in `ManuallyDrop` so [`Drop`] can release the underlying
    /// `heed::Env` BEFORE freeing our own registry slot. See the `Drop` impl
    /// for why the ordering is load-bearing.
    inner: ManuallyDrop<heed::Env>,
    canonical: PathBuf,
}

impl TrackedEnv {
    /// Open a new LMDB environment, registered in the global tracker.
    ///
    /// # Safety
    /// Same as `heed::EnvOpenOptions::open` — caller must ensure no other process
    /// opens the same path with incompatible options (different map_size or flags).
    pub unsafe fn open(
        opts: &heed::EnvOpenOptions,
        path: &Path,
        description: &str,
    ) -> Result<Self> {
        let canonical = register(path, description)?;

        match opts.open(path) {
            Ok(env) => Ok(Self {
                inner: ManuallyDrop::new(env),
                canonical,
            }),
            Err(e) => {
                unregister(&canonical);
                Err(e.into())
            }
        }
    }
}

impl Drop for TrackedEnv {
    fn drop(&mut self) {
        // Ordering here is load-bearing. heed maintains its OWN process-global
        // registry of opened environments (`OPENED_ENV`), keyed by canonical
        // path, that outlives a `heed::Env` until its last strong ref drops.
        // If we `unregister()` from our registry FIRST and let the field drop
        // afterwards (the default Rust drop order: body, then fields), there is
        // a window where our slot is free but heed's env is still alive. A
        // concurrent `TrackedEnv::open` on the same path — e.g. the idle reaper
        // dropping a repo while a reindex/query reopens it — then passes our
        // `register()` guard and falls through to `opts.open()`, which heed
        // rejects with the cryptic "an environment is already opened with
        // different options" (once a prior MDB_MAP_FULL resize left the live
        // env's recorded map_size differing from the reopen's resolved size).
        //
        // Dropping the `heed::Env` BEFORE `unregister()` enforces the invariant
        // "our slot free ⟹ heed's slot free": a concurrent open either sees our
        // slot still occupied (clear "double-open prevented" + retry) or sees
        // both free (clean reopen). It can never observe the inconsistent state
        // that produces heed's raw error.
        //
        // SAFETY: `inner` is dropped exactly once, here, and never accessed
        // again (the surrounding `TrackedEnv` is being destroyed).
        unsafe {
            ManuallyDrop::drop(&mut self.inner);
        }
        unregister(&self.canonical);
    }
}

impl Deref for TrackedEnv {
    type Target = heed::Env;
    fn deref(&self) -> &heed::Env {
        &self.inner
    }
}

impl std::fmt::Debug for TrackedEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackedEnv")
            .field("path", &self.canonical)
            .finish()
    }
}

// ── Tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_opts() -> heed::EnvOpenOptions {
        make_opts_sized(1024 * 1024)
    }

    fn make_opts_sized(map_size: usize) -> heed::EnvOpenOptions {
        let mut opts = heed::EnvOpenOptions::new();
        opts.map_size(map_size).max_dbs(1);
        opts
    }

    #[test]
    fn test_registry_prevents_double_open() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();
        let opts = make_opts();

        // First open should succeed
        let _env1 = unsafe { TrackedEnv::open(&opts, path, "test-1").unwrap() };

        // Second open on same path should fail
        let result = unsafe { TrackedEnv::open(&opts, path, "test-2") };
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("double-open prevented"));
        assert!(err.contains("test-1"));
    }

    #[test]
    fn test_registry_allows_reopen_after_drop() {
        let dir = TempDir::new().unwrap();
        let path = dir.path();
        let opts = make_opts();

        {
            let _env1 = unsafe { TrackedEnv::open(&opts, path, "test-1").unwrap() };
            // env1 dropped here
        }

        // Should succeed after drop
        let _env2 = unsafe { TrackedEnv::open(&opts, path, "test-2").unwrap() };
    }

    #[test]
    fn test_different_paths_both_allowed() {
        let dir1 = TempDir::new().unwrap();
        let dir2 = TempDir::new().unwrap();
        let opts = make_opts();

        let _env1 = unsafe { TrackedEnv::open(&opts, dir1.path(), "test-1").unwrap() };
        let _env2 = unsafe { TrackedEnv::open(&opts, dir2.path(), "test-2").unwrap() };
    }

    /// Regression guard for the `Drop` ordering bug: a previous `TrackedEnv`
    /// must release heed's internal `OPENED_ENV` slot BEFORE freeing our own
    /// registry slot. Otherwise a concurrent open that wins our `register()`
    /// race falls through to `opts.open()` and heed rejects it with
    /// "an environment is already opened with different options" — the exact
    /// production symptom (the two opens here request DIFFERENT map sizes so a
    /// stale-but-live heed env would mismatch).
    ///
    /// This test churns open→drop→reopen on a single shared path from multiple
    /// threads. It asserts the forbidden heed string NEVER appears. Our own
    /// "double-open prevented" error IS allowed (it means `register()` correctly
    /// serialized the racing opens). The assertion can only fail on a real
    /// regression — a missed race is a false negative, never a flaky failure.
    #[test]
    fn test_drop_releases_heed_slot_before_registry_slot() {
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let path: Arc<std::path::PathBuf> = Arc::new(dir.path().to_path_buf());

        let threads: Vec<_> = (0..4)
            .map(|t| {
                let path = Arc::clone(&path);
                std::thread::spawn(move || {
                    // Alternate map sizes per thread so any stale-but-live heed
                    // env observed by a racing open is guaranteed to mismatch
                    // options and surface BadOpenOptions if the bug is present.
                    let map_size = if t % 2 == 0 {
                        1024 * 1024
                    } else {
                        2 * 1024 * 1024
                    };
                    for _ in 0..50 {
                        let opts = make_opts_sized(map_size);
                        match unsafe { TrackedEnv::open(&opts, &path, "race") } {
                            Ok(env) => drop(env),
                            Err(e) => {
                                let msg = e.to_string();
                                assert!(
                                    !msg.contains("already opened with different options"),
                                    "heed slot leaked past our registry slot: {msg}"
                                );
                                // "double-open prevented" is the expected,
                                // benign outcome of a serialized race.
                            }
                        }
                    }
                })
            })
            .collect();

        for h in threads {
            h.join().unwrap();
        }
    }
}
