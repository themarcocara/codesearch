# Handoff: wiring this codesearch fork's Haxe support into a Kha-based project

Written for whichever agent is tasked with integrating **this codesearch
fork** (not upstream `flupkede/codesearch`) into a *different* project's CI
workflow and local developer setup. You have no memory of the work that
produced this document. Everything you need is below, or in the source
referenced inline — read the referenced files directly rather than trusting
paraphrase where precision matters (env var names, exact paths).

## What this is for

This fork of `codesearch` (a local semantic + symbol-aware code search tool)
adds Haxe language support: semantic chunking for text search, and a
`find_impact` capability that returns real, type-resolved "find all
references" results for Haxe symbols by shelling out to the Haxe compiler's
own `haxe --display <file>@<offset>@usage` mode — not a syntax-only
approximation. See `themarcocara/codesearch`:
- PR #1 (merged) — Haxe chunking + `find_impact` support.
- PR #2 — `HAXE_STD_PATH` auto-detection for portable/vendored Haxe SDKs,
  plus CI wiring. This document assumes PR #2's code is in whatever
  codesearch build you're deploying — if it isn't yet merged, check its
  status first, since the auto-detection behavior described below (only
  needing one env var, not two) depends on it.

**Your project already uses `Kode/kha`** (a Haxe game framework), and per
codesearch's own investigation, Kha does **not** rely on a system-wide
`haxe` install — it vendors its own per-platform Haxe SDK as a git submodule
(`Tools/<platform>/`, resolved by Kha's `khamake` build tool). The
reasonable assumption — confirmed to work — is that codesearch should read
Haxe from that same vendored copy, not require a second, separate Haxe
install just for itself.

## Getting codesearch itself

This is a Rust binary; you'll be building from source (`themarcocara/codesearch`
at whatever commit includes PR #1 + PR #2) until/unless a packaged release
exists. Pin to a specific commit SHA for your own CI reproducibility — don't
float on a branch tip.

```bash
git clone https://github.com/themarcocara/codesearch
cd codesearch
git checkout <pinned-commit-sha>
cargo build --release
```

**Heads-up on build prerequisites**: codesearch depends on the `ort` crate
(ONNX Runtime bindings, used for embeddings), whose build script downloads a
prebuilt binary from `cdn.pyke.io` on first build. If your CI runner has a
restrictive egress/firewall policy, confirm that host is reachable — this is
unrelated to Haxe specifically, but worth checking before assuming a build
failure is Haxe-related.

## How Haxe `find_impact` actually works (why the SDK wiring matters)

`src/symbols/haxe.rs`'s `HaxeSymbolIndexer` needs, at query time:
1. A `haxe` compiler binary (resolved via `CODESEARCH_HAXE` env var, or
   `$PATH` if unset).
2. That binary's standard library (`std/`) to be discoverable — either via
   `HAXE_STD_PATH`, or (as of PR #2) auto-detected if `std/` sits as a
   *sibling directory* next to the resolved `haxe` binary — which is
   exactly Kha's own vendored layout.
3. A `build.hxml` (or exactly one top-level `*.hxml`) in the indexed repo's
   root, so codesearch knows what classpath/libraries to compile against.
   If your Haxe project doesn't have a `build.hxml` at its root today,
   `find_impact` for it won't work until one exists there (a symlink or a
   thin wrapper `.hxml` that `-cp`s your real source root is fine).

If any of these are missing, `find_impact` for Haxe is silently disabled —
every other codesearch feature (including Haxe *text* search) keeps working
regardless, per the project's stated design (`.claude/CLAUDE.md`: missing
symbol tooling disables that one feature, never the rest).

## Local dev wiring

Find your project's own Kha checkout — wherever it's vendored (a git
submodule, a fetched dependency, however your build already locates it) —
and point `CODESEARCH_HAXE` at the `haxe` binary inside its
`Tools/<platform>/` directory. `sysdir()` (Kha's own platform-name logic,
`khamake/src/exec.ts`) maps:

| OS / arch | Directory | Binary |
|---|---|---|
| Linux x64 | `Tools/linux_x64/` | `haxe` |
| Linux arm64 | `Tools/linux_arm64/` | `haxe` |
| macOS x64 | `Tools/macos_x64/` | `haxe` |
| macOS arm64 | `Tools/macos_arm64/` | `haxe` |
| Windows (any arch) | `Tools/windows_x64/` | `haxe.exe` |

(Windows has no separate arm64 variant — Kha always uses `windows_x64`
regardless of host CPU.)

**Linux/macOS (bash/zsh):**
```bash
export CODESEARCH_HAXE=/path/to/your/kha-checkout/Tools/linux_x64/haxe
# HAXE_STD_PATH is auto-detected — Tools/linux_x64/std sits right next to
# the binary in Kha's layout. Don't set it manually unless you've moved
# the binary away from its sibling std/ (at which point auto-detection
# can't find it and you're on your own for setting it correctly).
```

**Windows (PowerShell — the default on `windows-latest` GitHub Actions
runners and most modern local setups):**
```powershell
$env:CODESEARCH_HAXE = "C:\path\to\your\kha-checkout\Tools\windows_x64\haxe.exe"
```

**Windows (cmd.exe):**
```cmd
set CODESEARCH_HAXE=C:\path\to\your\kha-checkout\Tools\windows_x64\haxe.exe
```

### Windows-specific gotcha: DLL colocation

Confirmed by inspecting `haxe.exe`'s PE import table directly (`objdump -p`):
unlike the Linux/macOS builds (which only depend on universally-present
system libraries), Windows' `haxe.exe` dynamically links against **five
non-system DLLs shipped alongside it in the same directory**:

```
libmbedcrypto.dll, libmbedtls.dll, libmbedx509.dll   (TLS, for haxelib)
libpcre2-8-0.dll                                      (regex / EReg)
zlib1.dll                                             (compression)
```

**Do not copy or symlink `haxe.exe` alone to some other location** — it
needs those five DLLs in the same directory (Windows' default DLL search
order checks the executable's own directory first). As long as you point
`CODESEARCH_HAXE` directly at the file inside Kha's own
`Tools/windows_x64/` directory without moving it, this is a non-issue —
only relevant if some deployment step relocates the binary.

## CI wiring

Don't check out the whole `Kode/kha` superproject in CI just to get Haxe —
it also pulls in Kore (the native engine), `khamake`, `khacpp`, and every
platform's Tools submodule. Fetch **only** the one `Kode/KhaTools_<platform>`
repo you need, and **only** the `haxe`(`.exe`) + `std/` paths inside it, via
a sparse partial clone. Verified concretely: this fetches ~57MB instead of
the ~110MB a full clone would (that repo also ships ~22MB of .NET/Java
target libs (`netlib/`, `hxjava/`) that a partial clone with a narrow
`sparse-checkout` skips entirely over the network, not just on disk).

### The critical part: pin to the SAME Haxe version your project's Kha uses

`Kode/KhaTools_<platform>` are **standalone repos with their own commit
history**, continuously updated in place whenever Kha bumps its Haxe
version — they are not versioned by tag to match Kha releases. The
correct, reproducible pin is **your own project's exact submodule
reference**, not some arbitrary recent commit. Get it like this, from your
own project's Kha checkout (or the pinned Kha version/tag your project
depends on):

```bash
git -C /path/to/your/kha-checkout ls-tree HEAD Tools/linux_x64
# 160000 commit <SHA>	Tools/linux_x64
```

That `<SHA>` is what you pin in CI — it guarantees CI's Haxe compiler
exactly matches what your local dev environment (and your project's actual
Kha version) uses. Do this per-platform (`Tools/linux_x64`,
`Tools/windows_x64`, etc.) since each has its own independent commit
history in its own standalone repo.

### The recipe (Linux CI runner)

```bash
# Do NOT use `git clone --depth 1` followed by `git checkout <sha>` --
# verified this fails once <sha> is no longer the CURRENT tip of the
# remote's default branch (which WILL happen, since these repos are
# updated in place). Clone --no-checkout, then fetch the pinned commit
# explicitly, which works regardless of where the branch tip has moved.
git clone --filter=blob:none --sparse --no-checkout \
  https://github.com/Kode/KhaTools_linux_x64.git haxe-sdk
cd haxe-sdk
git fetch --depth 1 origin <SHA-from-your-project's-Tools/linux_x64-gitlink>
git checkout FETCH_HEAD
git sparse-checkout set std
chmod +x haxe
./haxe --version   # sanity check -- doesn't need HAXE_STD_PATH

export CODESEARCH_HAXE="$PWD/haxe"
# HAXE_STD_PATH deliberately not set -- auto-detected from the sibling std/
```

### The recipe (Windows CI runner, PowerShell)

Same mechanism, different repo (`Kode/KhaTools_windows_x64`) and binary
name:

```powershell
git clone --filter=blob:none --sparse --no-checkout `
  https://github.com/Kode/KhaTools_windows_x64.git haxe-sdk
cd haxe-sdk
git fetch --depth 1 origin <SHA-from-your-project's-Tools/windows_x64-gitlink>
git checkout FETCH_HEAD
git sparse-checkout set std
.\haxe.exe --version

$env:CODESEARCH_HAXE = "$PWD\haxe.exe"
```

Remember the Windows DLL gotcha above — the sparse-checkout's default cone
mode already pulls in top-level files (which is where the 5 DLLs live), so
this works as-is; just don't add a step that copies `haxe.exe` out to
somewhere else without them.

`codesearch`'s own CI (`.github/workflows/ci.yml`, `haxe-integration-tests`
job) is a working, tested example of the Linux recipe end-to-end — copy its
shape directly if useful, adjusting only the pinned SHA to match your
project's Kha version rather than codesearch's own (which tracks whatever
`Kode/KhaTools_linux_x64`'s `main` was at the time that job was written, not
necessarily your project's pinned Kha version).

## Verifying the wiring end-to-end

Once `CODESEARCH_HAXE` is set (locally or in CI), a minimal sanity sequence
using the `codesearch` binary you built:

```bash
codesearch index add /path/to/your/haxe/project     # register the repo
codesearch index reindex <alias> --symbols --force  # trigger a symbol rebuild
```

(`<alias>` defaults to the directory name — `codesearch index list` shows
registered aliases if unsure.) Then drive `find_impact` via codesearch's MCP
server (however your agent tooling is set up to call it) against a known
symbol in your project, and confirm it returns real reference locations
rather than an error about a missing helper or a missing `.hxml`.

Note: the exact CLI flag names above were checked directly against
`src/cli/mod.rs` in this codebase, but the full end-to-end flow (a real
`find_impact` call against a real project through this exact CLI path) was
not exercised in the environment this handoff was written in — treat the
first real run as the actual validation, not this document.

## Known limitations to carry forward (not blockers, just be aware)

- **Cold compiler invocation per query.** Each `find_impact` call spawns a
  fresh `haxe --display` process, which recompiles the project's type graph
  from scratch. Fine for small-to-medium projects; noticeably slower on
  large ones. Haxe's own `--wait`/`--connect` compilation-cache server
  would fix this (measured ~10x faster warm, in the investigation that
  produced this fork's Haxe support) but isn't wired up yet — see
  `HAXE_FIND_IMPACT_TIER_A_HANDOFF.md` in this repo for that scoped
  follow-up if it becomes a real pain point for your project's size.
- **No name-based symbol search primitive.** `find_impact` by symbol *name*
  (as opposed to file+line position) falls back to a syntactic
  tree-sitter scan for a matching declaration name across the project,
  which can pick the wrong same-named declaration across unrelated types.
  Position-based lookups (file + line) don't have this limitation — they
  go straight through the compiler's real semantic resolution.
- **A known upstream tree-sitter-haxe grammar gap** affects semantic
  *chunking* (not `find_impact`): plain non-`enum` `abstract Name(Underlying)
  {}` declarations aren't recognized as their own node by the grammar this
  fork depends on (`themarcocara/tree-sitter-haxe`), and produce a parse
  error node instead — such declarations won't chunk as named symbols for
  text search. If your project uses `abstract` types heavily, expect
  slightly worse chunk boundaries around them specifically; everything else
  chunks normally. See `HAXE_INTEGRATION_HANDOFF.md` in this repo for
  details if you want to fix the grammar itself.
