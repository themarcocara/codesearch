# Index-status diagnostics: 0-chunk bug + TUI i/d/f

**Status:** 🟠 Belangrijk
**Prioriteit:** Niet urgent
**Spec:** Vastgesteld tijdens debug-sessie — `status projects` rapporteert 0 chunks voor repos die wél geïndexeerde data hebben (LMDB + SCIP geven content terug). Plus drie nieuwe TUI-keys gevraagd.
**Eigenaar:** OpenCode
**Branch:** fix/index-diagnostics

---

## 1. Probleem

`codesearch status --projects` (MCP `list_projects`) rapporteert `total_chunks: 0` en `total_files: 0` voor bepaalde repos terwijl die repos aantoonbaar geïndexeerde data bevatten. Concreet waargenomen: BAYR.Aprimo en BOIN.Aprimo tonen 0 chunks, maar `find_impact` en group-queries geven echte, precieze resultaten uit diezelfde repos terug. HUSQ.Aprimo toont daarentegen wél de correcte 12.248 chunks. De telling is dus onbetrouwbaar; de onderliggende data bestaat wel degelijk.

Root cause (sterk onderbouwd, één verificatiestap resteert): in serve-mode leest `list_projects` (`src/mcp/mod.rs:6941`) voor repos die níet open staan in de DashMap de counts uit `metadata.json` via `read_metadata_stats` (`src/mcp/mod.rs:2507`). Die functie defaultt naar `(0, 0)` zodra de sleutel `total_chunks` ontbreekt of het bestand niet parseert. De live telling (`VectorStore::stats()`, `src/vectordb/store.rs:655`, telt `self.chunks.len()`) wordt voor niet-geopende repos bewust nooit aangeroepen ("do NOT open the DB").

Het beslissende bewijs dat het om de metadata gaat en niet om een lege DB: de getroffen repos tonen een **echte `model`-naam** (`minilm-l6-q`) náást 0 chunks. Dus `metadata.json` bestaat en bevat het model-veld, maar mist/nulde `total_chunks`. Dat wijst op een writer die `metadata.json` overschrijft zonder de stats-velden te bewaren (model-metadata-write klobbert de stats-write, of omgekeerd), of op repos die geïndexeerd zijn voordat `update_metadata_stats` (`src/index/mod.rs:52`) de counts wegschreef.

### 1.1 Bewijs

| Repo | `status projects` | Werkelijke data | Conclusie |
|---|---|---|---|
| HUSQ.Aprimo | 12.248 chunks, model `minilm-l6-q` | `find_impact TokenManagementService` → refs, index-age ~5d | metadata.json correct |
| BAYR.Aprimo | 0 chunks, model `minilm-l6-q` | `find_impact Exporter` → definitie + 6 refs, index-age ~32d | metadata.json mist counts, LMDB heeft data |
| BOIN.Aprimo | 0 chunks | (te verifiëren) | vermoedelijk zelfde oorzaak |

## 2. Oplossing

Tweeledig. (A) De tel-bug: maak alle schrijfacties naar `metadata.json` een read-modify-write merge zodat stats- en model-velden elkaar nooit meer overschrijven, en schrijf atomisch (temp + rename). Voeg een defensieve fallback toe in `read_metadata_stats`: als `total_chunks == 0` maar de LMDB niet leeg is, open de store read-only en gebruik de live `stats()`. Eenmalige backfill via `doctor --fix`. (B) Drie TUI-keys in de lokale TUI die diagnose en herstel direct vanuit de repo-lijst mogelijk maken.

Verifieer eerst de root cause vóór je codet: open de `metadata.json` van een getroffen repo on-disk (`<repo>\<DB_DIR_NAME>\metadata.json`) en bevestig dat `total_chunks` ontbreekt of 0 is terwijl de LMDB chunks bevat. Identificeer daarna welke writer het bestand laatst overschrijft (zoek alle schrijvers van `metadata.json`: `update_metadata_stats` en de model-metadata-writer).

### 2.1 Dataflow (nieuw, TUI)

```
repo-lijst (run_tui) → toets ingedrukt → handle_key → KeyAction
  'i' → ShowInfo(idx)      → resolve idx→alias → stats()+db-size+age → modal overlay
  'd' → RunDoctor(idx)     → doctor::diagnose(Some(alias)) → DoctorReport → modal overlay
  'f' → ForceReindex(idx)  → tokio::spawn reindex(alias, force) → row-status "reindexing…" → on done: refresh stats
```

## 3. Concrete wijzigingen

### 3.1 `src/mcp/mod.rs`
- `read_metadata_stats` (regel ~2507): voeg live-fallback toe — bij `total_chunks == 0` en bestaande, niet-lege LMDB, open read-only en gebruik `VectorStore::stats()`. Houd dit lazy (alleen bij 0) om 15+ repos niet onnodig te openen.
- `list_projects` (regel ~6941): geen gedragswijziging nodig zodra `read_metadata_stats` corrigeert; controleer wel het stdio-pad (regel ~7032) op dezelfde fout.

### 3.2 metadata.json-writers (`src/index/mod.rs` + model-metadata-writer)
- `update_metadata_stats` (regel ~52): herschrijf naar read-modify-write merge + atomic write (schrijf naar `metadata.json.tmp`, dan rename).
- Pas de model-metadata-writer identiek aan zodat hij `total_chunks`/`total_files` niet wegvaagt.

### 3.3 `src/cli/doctor.rs`
- Splits diagnose van rendering. Nieuwe functie:
```rust
pub async fn diagnose(repo: Option<String>) -> Result<DoctorReport>;
```
- Laat de bestaande `run(fix, json, all, repo)` (regel ~617) deze gebruiken (geen duplicatie). Dit is nodig omdat `run` naar stdout print — in raw-mode TUI corrumpeert dat het scherm.

### 3.4 `src/serve/tui.rs`
- Vervang het `bool`-retour van `handle_key` (regel ~185) door een `KeyAction`-enum (zie §4). De `'s'`→reload-tak wordt `KeyAction::Reload`.
- Voeg arms toe voor `'i'`, `'d'`, `'f'` die de geselecteerde rij-index meegeven.
- In `run_tui` (regel ~36): match op `KeyAction`; hier is `serve_state` + repo-lijst in scope om idx→alias te mappen en de acties uit te voeren. Render modals (info / doctor) als overlay; reindex via `tokio::spawn` zodat de event-loop responsive blijft.
- Werk de help-footer bij: `i info · d doctor · f reindex · s reload · q quit`.

### 3.5 reindex-trigger
- Hergebruik het bestaande force-reindex-pad i.p.v. een nieuwe schrijver te openen. Onderzoek of de serve-watcher al een interne reindex-trigger heeft en roep die aan; coördineer via `serve_state` om lock-conflicten met de open store te vermijden.

## 4. Schema / DTOs

```rust
// src/serve/tui.rs
enum KeyAction { None, Quit, Reload, ShowInfo(usize), RunDoctor(usize), ForceReindex(usize) }

// src/serve/tui.rs — wat 'i' toont (afgeleid, geen nieuwe persistente struct nodig)
struct RepoDetail {
    alias: String, total_chunks: usize, total_files: usize,
    db_size_bytes: u64,            // som van bestanden onder DB_DIR
    model: String, dimensions: usize, max_chunk_id: u32,
    lock_status: String, index_age_secs: u64,
}

// src/cli/doctor.rs
struct DoctorReport { /* checks: Vec<(naam, status, detail)>, samenvatting */ }
```

## 5. Tests

**Unit**
- `read_metadata_stats_falls_back_to_live_count_when_zero` — metadata met `total_chunks:0` + niet-lege store → geeft live count.
- `metadata_write_merges_and_preserves_stats` — schrijf model, daarna stats, daarna model opnieuw → stats blijven behouden.
- `metadata_write_is_atomic` — onderbroken write laat geen 0-counts achter.
- `handle_key_maps_idfs_to_actions` — i/d/f/s/q → juiste `KeyAction`.
- `handle_key_noop_on_empty_list` — `row_count == 0` → `KeyAction::None`, geen panic.

**Integration**
- `status_reports_correct_counts_after_fix` — geïndexeerde repo met geklobberde metadata → status toont correcte counts.
- `doctor_diagnose_returns_structured_report` — `diagnose(Some(alias))` levert checks zonder naar stdout te printen.

**Handmatig**
- Start `codesearch serve`, open TUI, selecteer BOIN/BAYR: `i` toont niet-nul chunks; `d` toont doctor-rapport zonder schermcorruptie; `f` herindexeert en de rij-count wordt correct; `q` quit netjes.

## 6. Edge cases

| Scenario | Verwacht gedrag |
|---|---|
| Lege repo-lijst (`row_count == 0`) | i/d/f doen niets, geen panic |
| `'f'` terwijl serve de store open heeft | Coördineer via serve_state; geen lock-conflict, TUI blokkeert niet |
| `'f'` terwijl watcher die repo al herindexeert | Niet dubbel starten; toon bestaande status |
| `'d'` rapport langer dan scherm | Modal scrollbaar of nette afkap |
| `'i'` op repo zonder index (`db_path` bestaat niet) | Toon "niet geïndexeerd", niet 0/0 alsof het klopt |
| metadata.json mist `total_chunks` maar LMDB heeft data | Live-fallback toont echte count |
| Reindex faalt halverwege | Rij toont error-status; metadata blijft consistent (atomic) |

## 7. Definition of Done

- [ ] Root cause on-disk bevestigd (metadata.json van getroffen repo) en in dit bestand genoteerd
- [ ] Alle metadata.json-writers gebruiken read-modify-write merge + atomic write
- [ ] `status projects` toont correcte counts voor BOIN/BAYR/DMNT
- [ ] `'i'` toont chunks, files, db-grootte, model, dims, max_chunk_id, lock, index-age
- [ ] `'d'` draait doctor voor de geselecteerde repo en toont het rapport zonder terminalcorruptie
- [ ] `'f'` forceert reindex (non-blocking) met status-update en refresh
- [ ] Help-footer toont i/d/f
- [ ] Tests groen
- [ ] Handmatige eindtest doorlopen
- [ ] AGENTS_index-diagnostics.md bijgewerkt met de bevindingen

## 8. Niet in scope

- `src/serve/tui_remote.rs` (remote TUI): doctor en reindex zijn lokale operaties; remote vereist een RPC-laag — aparte taak. Expliciet niet meenemen, anders bouwt de agent ongevraagd RPC.
- Wijzigingen aan het indexeer-/embedding-algoritme zelf.
- SCIP-index/`find_impact` gedrag — dat werkt correct; enkel niet verwarren met de embedding-metadata in de `'i'`-weergave.

## 9. Implementatietijd

| Onderdeel | Schatting |
|---|---|
| Root cause verifiëren + writer-merge + atomic + backfill | ~2-3u |
| `read_metadata_stats` live-fallback | ~1u |
| KeyAction-refactor in tui.rs + event-loop | ~1u |
| `'i'` info-modal | ~1,5u |
| `'d'` doctor diagnose-variant + modal | ~2u |
| `'f'` force reindex (concurrency met serve) | ~2-3u |
| Tests | ~1,5u |
| **Totaal** | **~11-13u** |

## 10. Review-opmerkingen

- Grootste risico is `'f'` tegen een draaiende serve: de store kan open staan in de DashMap; een reindex die de LMDB-env heropent kan conflicteren. Hergebruik bij voorkeur de bestaande serve-watcher reindex-trigger i.p.v. een tweede writer te openen.
- `doctor::run` print naar stdout; in raw-mode TUI corrumpeert dat het scherm. De diagnose/rendering-split is daarom verplicht, niet optioneel.
- Voorkeur voor de writer-merge als échte fix (robuust) boven status altijd de DB laten openen (traag bij 15+ repos). De live-fallback is enkel een vangnet bij count 0.
- metadata.json-write moet atomic (temp + rename), anders herintroduceer je 0-counts bij een onderbroken write.
- De index-age die `find_impact` (SCIP) rapporteert is een aparte index dan de embedding-metadata; als je SCIP-leeftijd in `'i'` toont, label beide leeftijden apart.

## 11. Commit message voorstel

```
fix(serve): correct chunk counts in status + add i/d/f TUI diagnostics

list_projects reported 0 chunks for repos whose metadata.json had been
overwritten without preserving stats, while the LMDB store held real data.
Metadata writes now merge fields atomically with a live-count fallback, and
the local TUI gains info (i), doctor (d) and force-reindex (f) actions.

Closes: AGENTS_index-diagnostics.md
```
