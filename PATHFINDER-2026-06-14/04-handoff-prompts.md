# Handoff Prompts — gsd-dashboard

Copy any block below directly into `/make-plan`. Each targets one unified system from `03-unified-proposal.md`, lists the exact call sites to rewrite, cites the flowchart, and carries system-specific anti-pattern guards. Recommended order: U1 → U2 → (U3, U4, U5 in parallel). U1 lands first because U2/U3 build on it.

---

## U1 — Write-transaction helper

```
/make-plan Introduce a single write-transaction helper and route every multi-statement SQLite write through it.

Target: new `with_write_txn<T>(conn, body)` in src-tauri/src/store/mod.rs that owns begin/commit/map_err(AppError::from). See PATHFINDER-2026-06-14/01-flowcharts/F1-scan-pipeline.md (persist section) and F2-session-telemetry.md (repo section).

Rewrite these call sites to call the helper instead of hand-rolling the transaction shell:
- src-tauri/src/store/project_repo.rs:66-165 (upsert_project_snapshot)
- src-tauri/src/sessions/repo.rs:200-219 (persist_indexed_file_result)
- src-tauri/src/store/daily_activity.rs:24-26 (daily activity rebuild)
- src-tauri/src/store/settings_repo.rs:50-57 (save_settings upsert)
Also collapse the three prune_* DELETE wrappers (sessions/repo.rs:258-265, 266-281, 282-304) to a 3-line execute_delete(tx, sql) or inline tx.execute inside a closure.

Guards:
- Do NOT add a Repository trait, generic ORM layer, or query builder. One free function only.
- Preserve exact SQL and params! at every site; this is a pure shell extraction, behavior-identical.
- Keep deadpool interact() boundaries where they are; the helper operates on the &mut Connection inside interact.
- Verify: existing store/session tests pass unchanged; WAL mode and migrations untouched.
```

---

## U2 — Shared per-item ingest (scan ↔ watcher)

```
/make-plan Extract one per-item ingest function shared by the full IPC scan and the incremental file watcher so parse-tolerance and persistence can't drift between them.

Target: ingest_one_project(state, candidate) and ingest_one_session(state, path) in src-tauri/src/scan_service.rs, each doing parse + persist and returning an outcome (id + parse issues) for the caller to emit. See PATHFINDER-2026-06-14/01-flowcharts/F1-scan-pipeline.md and F3-file-watching.md.

FIRST verify whether watcher/refresh.rs already delegates to scan_service per-candidate logic:
- src-tauri/src/watcher/refresh.rs:18-40 (refresh_project_planning_dir_for_app)
- src-tauri/src/watcher/refresh.rs:42-60 (refresh_session_file)
If it already delegates, STOP and report D2 as already-resolved. If it reimplements parse/persist, proceed.

Rewrite call sites to use the shared functions:
- Full scan loop body: src-tauri/src/scan_service.rs:51-89 (calls ingest_one_project, then emits ScanEvent)
- Watcher project refresh: watcher/refresh.rs:18-40 (calls ingest_one_project, emits ProjectUpdated at watcher/runtime.rs:126)
- Watcher session refresh: watcher/refresh.rs:42-60 (calls ingest_one_session, emits SessionNew at watcher/runtime.rs:152)

Guards:
- Event EMISSION stays at the call sites (realtime watcher vs IPC vs Channel progress are intentionally separate — do NOT centralize emit into the ingest fn).
- Do NOT introduce a "RefreshCoordinator" object or event bus; just one shared parse+persist fn per item type.
- Honor the read-only-against-.planning invariant — ingest never writes into .planning.
- Per-file parse failure must stay non-fatal (scan_log + issue badge), identical to today.
- Verify: editing a .planning file via watcher and a full rescan produce identical persisted snapshots.
```

---

## U3 — Metadata-parse helper

```
/make-plan Finish the read→parse→collect-issue abstraction in the scan pipeline.

Target: parse_metadata_file<T>(path, parse_fn, &mut issues) -> Option<T> in src-tauri/src/scan_service.rs, building on existing read_optional_or_issue. See PATHFINDER-2026-06-14/01-flowcharts/F1-scan-pipeline.md.

Collapse these four near-identical blocks to single calls:
- src-tauri/src/scan_service.rs:127-131 (roadmap, required)
- src-tauri/src/scan_service.rs:136-143 (milestones, optional)
- src-tauri/src/scan_service.rs:149-157 (state, optional)
- src-tauri/src/scan_service.rs:163-169 (config, optional)

Guards:
- required-vs-optional is a parameter/variant, not a second helper.
- Error display path (display_path()) and ParseIssue shape must be byte-identical to current output.
- Verify: scan fixtures with malformed ROADMAP/STATE/config produce the same parse_issues as before.
```

---

## U4 — Shared session-field applier

```
/make-plan Extract the common session-field logic shared by the Claude and Codex JSONL parsers.

Target: apply_common_session_fields(accumulator, &record) in src-tauri/src/sessions/mod.rs covering timestamp, cwd, source_session_id, message_count, model (set-if-unset) and token fold. See PATHFINDER-2026-06-14/01-flowcharts/F2-session-telemetry.md.

Rewrite:
- src-tauri/src/sessions/claude.rs:9-61 (parse_claude_record) — keep Claude-specific JSON paths, call the applier
- src-tauri/src/sessions/codex.rs:9-86 (parse_codex_record) — keep Codex-specific JSON paths, call the applier

Guards (CRITICAL — PITFALL #5, unstable external schemas):
- The applier MUST keep every field Option<T>-tolerant. Do NOT promote any field to required.
- Partial/last-line records must still parse to a partially-populated accumulator (byte-offset resume relies on this).
- Source-specific message_count filters stay in the per-source functions, not the shared applier.
- Verify: run against the multi-version Claude AND Codex fixtures; token totals and message counts unchanged for every fixture version.
```

---

## U5 — Frontend page hooks

```
/make-plan Extract two shared React hooks to remove duplicated query/mutation scaffolding from the page components.

Targets:
- useEventStreamMutation(mutationFn, setStatus) — owns the Channel + onMutate/onSuccess(invalidateQueries)/onError shell.
- useCommonPageQueries() — returns { settings, portfolio, saveSettings }.
See PATHFINDER-2026-06-14/01-flowcharts/F8-portfolio-views.md and F9-global-sessions.md.

Rewrite:
- src/routes/PortfolioPage.tsx:239-262 (useScanProjectsMutation) -> useEventStreamMutation
- src/routes/PortfolioPage.tsx:266-309 (useIndexSessionsMutation) -> useEventStreamMutation
- src/routes/PortfolioPage.tsx:319-325 -> useCommonPageQueries
- src/routes/GlobalSessionsPage.tsx:275-277 -> useCommonPageQueries

Guards:
- Do NOT touch src/lib/ipc.ts (keep the typed per-command wrappers) or src/lib/appListeners.ts (distinct per-event invalidation) — both were reviewed and are intentionally kept.
- Preserve exact query keys and invalidation predicates; the hook is a wrapper, not a behavior change.
- Keep the Channel<ScanEvent>/Channel<SessionIndexEvent> progress wiring intact.
- Verify: existing PortfolioPage/GlobalSessionsPage tests pass unchanged; live scan/index progress bars still update.
```
