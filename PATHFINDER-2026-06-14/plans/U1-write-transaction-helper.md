# Plan: U1 — Single write-transaction helper

**Goal:** Centralize the `begin → … → commit` shell behind one helper `store::with_write_txn`, and collapse the three single-DELETE prune wrappers behind a tiny `store::execute_delete`. Pure shell extraction — **behavior-identical**, same SQL, same `params!`.

**Source of truth:** grounded in a direct read of current source (2026-06-14), not the approximate pathfinder line numbers. Flowcharts: `../01-flowcharts/F1-scan-pipeline.md` (persist), `../01-flowcharts/F2-session-telemetry.md` (repo).

> ⚠️ This refactor's payoff is **consistency + one place to set transaction policy** (e.g. future retry-on-`SQLITE_BUSY`), not large LoC reduction. Each multi-statement site nets roughly even on line count. Do not oversell it; do it for the single chokepoint.

---

## Phase 0 — Findings & Allowed APIs (already gathered; read before editing)

### Allowed APIs (verified in-repo + rusqlite)
- `rusqlite::Connection::transaction(&mut self) -> rusqlite::Result<Transaction<'_>>`
- `rusqlite::Transaction::commit(self) -> rusqlite::Result<()>`
- `rusqlite::Connection::execute(&self, sql, params) -> rusqlite::Result<usize>` — `Transaction` derefs to `Connection`, so `&Transaction` coerces to `&Connection`.
- `impl From<rusqlite::Error> for AppError` — **exists** at `src-tauri/src/error.rs:73-77`. So `.map_err(AppError::from)` is correct everywhere.
- `connection.interact(closure)` closures receive **`&mut rusqlite::Connection`** (confirmed `store/mod.rs:37-43`). The helper operates *inside* that closure; **interact boundaries do not move.**

### Verified call-site inventory

| # | Function | File:line (fn start) | Txn? | Action |
|---|----------|----------------------|------|--------|
| A | `upsert_project_snapshot` | `store/project_repo.rs:60` (begin `:66`, commit `:165`, 3 execs) | yes | → `with_write_txn` |
| B | `clear_project_cache` | `store/project_repo.rs:268` (3 deletes) | yes | → `with_write_txn` *(discovered)* |
| C | `replace_plan_items` | `store/project_repo.rs:317` (delete + insert loop) | yes | → `with_write_txn` *(discovered)* |
| D | `rebuild_window` | `store/daily_activity.rs:19` (begin `:24`, commit `:26`) | yes | → `with_write_txn` (closure just calls `rebuild_window_in_transaction(tx, …)`) |
| E | `persist_indexed_file_result` | `sessions/repo.rs:200` (begin `:206`, commit `:219`, 1 direct exec + 2 inner) | yes | → `with_write_txn` |
| F | `prune_unmatched_sessions` | `sessions/repo.rs:258-263` | **no** (direct single DELETE) | → `execute_delete` |
| G | `prune_orphan_index_states` | `sessions/repo.rs:266-279` | **no** | → `execute_delete` |
| H | `prune_tokenless_codex_index_states` | `sessions/repo.rs:282-302` | **no** | → `execute_delete` |
| — | `save_settings` | `store/settings_repo.rs:50` | **no** (single `INSERT OR REPLACE`) | **EXCLUDED — see below** |

### Correction to the original U1 handoff
- **`settings_repo::save_settings` is NOT routed through the helper.** It is one atomic `INSERT OR REPLACE`, no current transaction. Wrapping it would *introduce* a BEGIN/COMMIT → not behavior-identical. Leave it untouched.
- **Two extra sites (B, C)** in `project_repo.rs` were found beyond the handoff's list. Including them satisfies "route *every* multi-statement write." Before editing, run the discovery grep in Phase 2 to confirm the full set and exact begin/commit lines for B and C.

### Anti-patterns to avoid
- ❌ Repository trait / generic ORM / query-builder. **Two free functions only.**
- ❌ Changing any SQL string, `params!`, or column list.
- ❌ Moving work into/out of `interact()`.
- ❌ Wrapping single-statement writes (`save_settings`, the prunes) in a transaction.
- ❌ A generic `params: impl Params` arg on `execute_delete` — all three prunes pass `[]`; keep it 2-arg.

---

## Phase 1 — Add the two helpers (no call sites yet)

**File:** `src-tauri/src/store/mod.rs`, insert after `configure_connection` (after line 71).

Copy this verbatim:

```rust
/// Runs `body` inside a single write transaction, owning begin/commit and
/// error mapping. The body receives the active transaction; it must NOT
/// commit. Any `Err` returned rolls back (transaction drop).
pub fn with_write_txn<T>(
    connection: &mut rusqlite::Connection,
    body: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, AppError>,
) -> Result<T, AppError> {
    let transaction = connection.transaction().map_err(AppError::from)?;
    let value = body(&transaction)?;
    transaction.commit().map_err(AppError::from)?;
    Ok(value)
}

/// Executes a single DELETE (or other row-count) statement with no params and
/// returns the affected row count. For standalone single-statement writes that
/// intentionally run outside a transaction. `&Transaction` also accepts here
/// via deref coercion if ever needed.
pub fn execute_delete(connection: &rusqlite::Connection, sql: &str) -> Result<i64, AppError> {
    connection
        .execute(sql, [])
        .map(|count| count as i64)
        .map_err(AppError::from)
}
```

**Note:** `rusqlite::Connection` is already imported (`store/mod.rs:4` imports `Connection`); fully-qualified paths above are fine and unambiguous either way. `AppError` is imported at `store/mod.rs:6`.

**Verify Phase 1:**
- `cargo build -p <tauri-crate>` (or `cd src-tauri && cargo check`) compiles with the new unused-fn warnings only.
- `grep -n "pub fn with_write_txn\|pub fn execute_delete" src-tauri/src/store/mod.rs` → 2 hits.

---

## Phase 2 — Confirm the full transaction set, then rewrite multi-statement sites (A–E)

### 2.0 Discovery (do this first)
```
grep -n "\.transaction()" src-tauri/src/store/project_repo.rs \
  src-tauri/src/store/daily_activity.rs src-tauri/src/sessions/repo.rs
grep -n "\.commit()" src-tauri/src/store/project_repo.rs \
  src-tauri/src/store/daily_activity.rs src-tauri/src/sessions/repo.rs
```
Confirm exactly sites A–E (5 functions). If grep surfaces additional `.transaction()` callers, add them with the SAME pattern. Note the exact begin/commit line for B and C from this grep before editing.

### 2.1 Rewrite pattern (apply to each of A–E)
For each function, the edit is mechanical:
- **Delete** the line `let transaction = connection.transaction().map_err(AppError::from)?;`
- **Delete** the trailing `transaction.commit().map_err(AppError::from)` (and return it via the helper instead).
- **Wrap** the body in `with_write_txn(connection, |transaction| { … ; Ok(()) })`.
- Keep every `transaction.execute(...).map_err(AppError::from)?` and every inner call (`upsert_indexed_session(transaction, …)`, `save_index_state(transaction, …)`, `rebuild_window_in_transaction(transaction, …)`) **unchanged** — they already take `&rusqlite::Transaction<'_>`, and the closure's `transaction` binding is exactly that.

**Worked example — A `upsert_project_snapshot` (`project_repo.rs:66-165`):**

Before:
```rust
let transaction = connection.transaction().map_err(AppError::from)?;
transaction.execute("INSERT INTO projects …", params![…]).map_err(AppError::from)?;
// … (DELETE phase_plans, INSERT phase_plans loop) …
transaction.commit().map_err(AppError::from)
```
After:
```rust
with_write_txn(connection, |transaction| {
    transaction.execute("INSERT INTO projects …", params![…]).map_err(AppError::from)?;
    // … (DELETE phase_plans, INSERT phase_plans loop) — unchanged …
    Ok(())
})
```

**Worked example — D `rebuild_window` (`daily_activity.rs:24-26`):**
```rust
with_write_txn(connection, |transaction| {
    rebuild_window_in_transaction(transaction, days, now_ms)
})
```
(`rebuild_window_in_transaction` already returns `Result<(), AppError>`, so no extra `Ok(())`.)

**Worked example — E `persist_indexed_file_result` (`sessions/repo.rs:206-219`):**
```rust
with_write_txn(connection, |transaction| {
    transaction
        .execute("DELETE FROM sessions WHERE source_path = ?1", [&state.source_path])
        .map_err(AppError::from)?;
    for session in sessions {
        upsert_indexed_session(transaction, session, now)?;
    }
    save_index_state(transaction, state, now)
})
```

### 2.2 Import the helper
In `project_repo.rs`, `daily_activity.rs`, `sessions/repo.rs`, add `use crate::store::with_write_txn;` (or call as `crate::store::with_write_txn`). Match the file's existing import style.

**Verify Phase 2:**
- `cargo check` clean (no unused `with_write_txn` warning now).
- `grep -n "\.commit()" src-tauri/src/store/ src-tauri/src/sessions/repo.rs` → **only** the single `.commit()` inside `with_write_txn` in `store/mod.rs`; zero in the rewritten files.
- `grep -n "connection.transaction()" src-tauri/src/store src-tauri/src/sessions` → only inside `with_write_txn`.

---

## Phase 3 — Collapse the prune wrappers (F, G, H)

**File:** `src-tauri/src/sessions/repo.rs`. Add `use crate::store::execute_delete;`.

Rewrite each prune body to a single call, keeping the exact SQL strings:
```rust
pub fn prune_unmatched_sessions(connection: &mut rusqlite::Connection) -> Result<i64, AppError> {
    execute_delete(connection, "DELETE FROM sessions WHERE project_id IS NULL")
}

pub fn prune_orphan_index_states(connection: &mut rusqlite::Connection) -> Result<i64, AppError> {
    execute_delete(
        connection,
        "DELETE FROM session_index_state
         WHERE NOT EXISTS (
            SELECT 1
            FROM sessions
            WHERE sessions.source_path = session_index_state.source_path
         )",
    )
}

pub fn prune_tokenless_codex_index_states(
    connection: &mut rusqlite::Connection,
) -> Result<i64, AppError> {
    execute_delete(
        connection,
        "DELETE FROM session_index_state
         WHERE source = 'codex'
            AND EXISTS (
                SELECT 1
                FROM sessions
                WHERE sessions.source_path = session_index_state.source_path
                    AND sessions.source = 'codex'
                    AND sessions.message_count > 0
                    AND COALESCE(sessions.tokens_in, 0) = 0
                    AND COALESCE(sessions.tokens_out, 0) = 0
            )",
    )
}
```
Signatures (`&mut Connection`) stay unchanged — `&mut` coerces to `&Connection` at the call. **Copy the SQL strings byte-for-byte from the current source; do not re-type them.**

**Verify Phase 3:**
- `cargo check` clean.
- `grep -n "\.map(|count| count as i64)" src-tauri/src/sessions/repo.rs` → 0 hits (all three folded into `execute_delete`).

---

## Phase 4 — Verification (prove behavior-identical)

1. **Build:** `cd src-tauri && cargo build` — no errors, no new warnings.
2. **Clippy:** `cargo clippy -- -D warnings` (if the project gates on it) — clean.
3. **Tests:** `cargo test` — the existing store/session tests pass **unchanged**. Do not edit any test. If a test fails, the extraction changed behavior → revert and diff.
4. **SQL integrity grep:** diff the SQL strings pre/post — `git diff` should show SQL lines only *moved*, never *modified* (no token changes inside any string literal).
5. **Invariants untouched:**
   - `grep -rn "PRAGMA journal_mode\|WAL\|wal" src-tauri/src/store/mod.rs` — WAL setup unchanged.
   - `git diff --stat src-tauri/src/store/migrations.rs` — **migrations.rs has zero changes.**
   - `git diff src-tauri/src/store/settings_repo.rs` — **zero changes** (save_settings deliberately excluded).
6. **Scope grep — only the helper holds a commit:**
   `grep -rn "\.commit()" src-tauri/src` → exactly one hit, in `store/mod.rs::with_write_txn`.
7. **Frontend smoke (optional, behavioral):** run the app, trigger a scan and a session index; project cards + session table populate as before (the persist paths E/A executed end-to-end).

**Done when:** all of A–H rewritten, settings_repo + migrations + tests untouched, single `.commit()` in the codebase, `cargo test` green.

---

## Commit guidance
One commit (or two: "feat(store): add with_write_txn + execute_delete helpers" then "refactor(store,sessions): route writes through transaction helpers"). Conventional-commit format, no co-author. The project is on branch `chore/bump-v0.1.8`; if a fresh branch is wanted, branch from main as `refactor/write-txn-helper` before editing.
```
