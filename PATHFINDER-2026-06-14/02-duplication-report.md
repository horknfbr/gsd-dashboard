# Duplication Report — gsd-dashboard

Synthesis of the within-feature and cross-feature duplication hunts (Phase 2). Every claim cites ≥2 `file:line`. Each concern is bucketed: **ACCIDENTAL** (consolidate), **LEGITIMATE SPECIALIZATION** (leave alone), or **REFUTED** (claim did not hold under inspection).

Verdicts are deliberately conservative — per the project's "simplicity first / no speculative abstraction" rule, a concern is only marked ACCIDENTAL when the variation between sites is purely mechanical.

---

## ACCIDENTAL — worth consolidating

### D1 — SQLite write-transaction boilerplate (cross-feature) ⭐ highest value
Every repo hand-rolls `connection.transaction() → execute(...).map_err(AppError::from)? × N → commit().map_err(AppError::from)`.
- `src-tauri/src/store/project_repo.rs:66-165` (`upsert_project_snapshot`, ~13 execs)
- `src-tauri/src/sessions/repo.rs:200-219` (`persist_indexed_file_result`, ~14 execs)
- `src-tauri/src/store/daily_activity.rs:24-26` (rebuild txn, ~4 execs)
- `src-tauri/src/store/settings_repo.rs:50-57` (`save_settings` upsert)

**Why mechanical:** only the SQL body and `params!` differ; the begin/`map_err`/commit shell is identical. ~80 LoC of pure shell.
**Action:** one `with_write_txn(conn, |tx| { ... })` helper in `store/mod.rs` that owns begin/commit/`map_err`.

### D2 — Incremental ingest reimplements full-scan ingest (cross-feature, F1 ↔ F3)
Two paths turn one `.planning` dir (or one `.jsonl` file) into a persisted snapshot + `ProjectUpdated`/`SessionNew` emit:
- Full scan: `src-tauri/src/scan_service.rs:111` (`read_and_parse_candidate`) → persist `scan_persistence.rs:35` → emit `scan_service.rs:81`
- Watcher incremental: `src-tauri/src/watcher/refresh.rs:18` (`refresh_project_planning_dir_for_app`) and `:42` (`refresh_session_file`) → emit `watcher/runtime.rs:126-165`

**Why mechanical (if confirmed):** both must do "parse one candidate → persist → emit". If `watcher/refresh.rs` re-implements parsing/persisting rather than calling the scan-service per-candidate function, the logic will drift (a parse-tolerance fix in one path won't reach the other).
**Action:** extract `ingest_one_project(candidate)` and `ingest_one_session(path)` as the single per-item function; both the IPC full-scan loop and the watcher refresh call it.
**Confidence:** MODERATE — needs a 5-min confirm that `refresh.rs` does not already delegate to `scan_service`'s per-candidate fn.

### D3 — `prune_*` DELETE wrappers (within F2)
Three functions are identical but for the SQL string:
- `src-tauri/src/sessions/repo.rs:258-265` (`prune_unmatched_sessions`)
- `src-tauri/src/sessions/repo.rs:266-281` (`prune_orphan_index_states`)
- `src-tauri/src/sessions/repo.rs:282-304` (`prune_tokenless_codex_index_states`)

Shell: `execute(sql) → map(|c| c as i64) → map_err(AppError::from)`. **Action:** `execute_delete(conn, sql) -> Result<i64>`. (~15 LoC; subsumed by D1's helper.)

### D4 — `read → parse → push issue` metadata blocks (within F1)
Four near-identical blocks dispatching each `.planning` file to its parser and collecting a `ParseIssue` on error:
- `src-tauri/src/scan_service.rs:127-131` (roadmap), `:136-143` (milestones), `:149-157` (state), `:163-169` (config)

**Action:** `parse_metadata_file<T>(path, parse_fn, &mut issues) -> Option<T>`. (`read_optional_or_issue` already exists — finish the abstraction.)

### D5 — Session common-field extraction across parsers (within F2)
`parse_claude_record` and `parse_codex_record` share ~30 LoC of timestamp/cwd/source_id/message_count/model set-if-unset logic:
- `src-tauri/src/sessions/claude.rs:9-61`
- `src-tauri/src/sessions/codex.rs:9-86`

**Action:** `apply_common_session_fields(accumulator, record)`; keep the source-specific JSON-path extraction separate.
**Guard:** these parsers absorb an unstable external schema (PITFALL #5) — the shared core must stay `Option<T>`-tolerant; do NOT let consolidation introduce required fields.

### D6 — Event-stream mutation + common-page-query hooks (within F8/F9)
- Mutations: `src/routes/PortfolioPage.tsx:239-262` (`useScanProjectsMutation`) and `:266-309` (`useIndexSessionsMutation`) — identical onMutate/onSuccess/onError + Channel scaffold.
- Page queries: `src/routes/PortfolioPage.tsx:319-325` and `src/routes/GlobalSessionsPage.tsx:275-277` — both load settings + portfolio + saveSettings.

**Action:** `useEventStreamMutation(mutationFn, statusSetter)` and `useCommonPageQueries()`.

---

## LEGITIMATE SPECIALIZATION — leave as-is

### S1 — Two parsing layers (F1 planning files vs F2 JSONL sessions)
- `src-tauri/src/parser/*` vs `src-tauri/src/sessions/{claude,codex,file_indexer}.rs`
Different **trust models**: planning files are user-owned, schema-stable, parse-error = surface to user; session JSONL is external/unstable (community-reversed), parse-error = tolerate-and-continue per-record with byte-offset resume. Merging them would couple a stable grammar to a defensively-tolerant streamer. **Keep separate.**

### S2 — Chart aggregation: project vs global (F8 vs F9)
- `src-tauri/src/sessions/project_charts.rs:48-150` (`WHERE project_id=?`, milestone velocity) vs `src-tauri/src/sessions/global.rs:181-223` (group-by source/project, time-of-day, day-of-week)
Different query scopes and output dimensions. Shared SQL idiom (date bucketing) is one line, not a system. **Keep separate.**

### S3 — Event-emit DI callback signature (`on_event: impl Fn(E) -> Result<...>`)
- `scan_service.rs:30`, `sessions/indexer.rs:43`, `watcher/refresh.rs:18,42`
Intentional dependency injection so services don't hold a Tauri `AppHandle` (testability). A repeated *signature* is not duplicated *logic*. An `EventEmitter` trait would be abstraction-for-its-own-sake. **Keep.**

### S4 — Tray state split / settings load-vs-save
- `tray/service.rs:78` vs `:96` (DB-I/O vs pure transform — aids unit testing)
- `settings.rs:77` vs `:99` (None-fallback vs always-persist — different control flow)
Both are correct single-responsibility splits, not duplication. **Keep.**

---

## REFUTED — claim did not hold

### R1 — Frontend `appListeners.ts` listener boilerplate
The cross-feature pass *inferred* (medium confidence) ~6 mechanical `listen()` wrappers. Direct read of `src/lib/appListeners.ts:42-90` refutes this: each listener carries distinct invalidation logic — `settings-changed` invalidates settings+portfolio+all-project (`:42-50`); `project:updated` does payload type-guard + 4 keyed invalidations + a sessions/charts predicate (`:54-70`); `session:new` fans to global/heatmap/project predicates (`:71-87`). Only the 6-line unlisten teardown (`:100-105`) is mechanical and trivial. **Not worth abstracting.**

### R2 — Frontend `ipc.ts` wrappers → generic registry
The cross-feature pass flagged 20 thin `invoke<T>()` wrappers (`src/lib/ipc.ts:32-167`) as accidental and proposed a `commands` config-map + generic `invokeCommand`. **Rejected:** each wrapper is the per-command type-safe surface (return type + named params + Channel wiring, e.g. `:63` `rebuildCache`). The proposed registry/factory trades compile-time typing for fewer lines and is exactly the anti-pattern the unification phase forbids. Mechanical ≠ worth replacing when the repetition *is* the type safety. **Keep the wrappers.**

---

## Priority

| Rank | Concern | Type | Effort | Payoff |
|------|---------|------|--------|--------|
| 1 | D1 transaction helper | backend | S | ~80 LoC + consistency; unblocks D3 |
| 2 | D2 shared per-item ingest | backend | M | prevents scan/watcher drift (correctness) |
| 3 | D6 frontend hooks | frontend | S | ~40 LoC, clearer pages |
| 4 | D4 metadata parse helper | backend | S | ~25 LoC |
| 5 | D5 session field extractor | backend | S | ~30 LoC (guard schema tolerance) |
| 6 | D3 prune helper | backend | XS | folds into D1 |
