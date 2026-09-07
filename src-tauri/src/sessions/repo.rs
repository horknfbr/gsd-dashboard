use std::collections::{BTreeMap, HashSet};

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::{
    error::AppError,
    sessions::{IndexedSession, ProjectRoot, SessionIndexState, SessionSource},
    store::{execute_delete, with_write_txn},
};

const DAY_MS: i64 = 86_400_000;
const RECENT_UNMATCHED_LIMIT: i64 = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioSessionSummary {
    pub sessions_today: i64,
    pub tokens_today: i64,
    pub sparkline_by_project: BTreeMap<String, [i64; 7]>,
    pub unmatched_count: i64,
    pub unmatched_claude_count: i64,
    pub unmatched_codex_count: i64,
    pub recent_unmatched: Vec<UnmatchedSessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmatchedSessionSummary {
    pub id: String,
    pub source: SessionSource,
    pub source_path: String,
    pub cwd: Option<String>,
    pub started_at: Option<i64>,
    pub model: Option<String>,
    pub index_error: Option<String>,
}

/// Inserts or updates a session record in the database.
pub fn upsert_indexed_session(
    transaction: &rusqlite::Transaction<'_>,
    session: &IndexedSession,
    now: i64,
) -> Result<(), AppError> {
    transaction
        .execute(
            "INSERT INTO sessions (
                id,
                source,
                source_path,
                source_session_id,
                project_id,
                cwd,
                started_at,
                ended_at,
                duration_ms,
                message_count,
                tokens_in,
                tokens_out,
                model,
                attribution_method,
                index_error,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?16)
            ON CONFLICT(id) DO UPDATE SET
                source = excluded.source,
                source_path = excluded.source_path,
                source_session_id = excluded.source_session_id,
                project_id = excluded.project_id,
                cwd = excluded.cwd,
                started_at = excluded.started_at,
                ended_at = excluded.ended_at,
                duration_ms = excluded.duration_ms,
                message_count = excluded.message_count,
                tokens_in = excluded.tokens_in,
                tokens_out = excluded.tokens_out,
                model = excluded.model,
                attribution_method = excluded.attribution_method,
                index_error = excluded.index_error,
                updated_at = excluded.updated_at",
            params![
                session.id,
                session.source.as_str(),
                session.source_path,
                session.source_session_id,
                session.project_id,
                session.cwd,
                session.started_at,
                session.ended_at,
                session.duration_ms,
                session.message_count,
                session.tokens_in,
                session.tokens_out,
                session.model,
                session.attribution_method,
                session.index_error,
                now,
            ],
        )
        .map(|_| ())
        .map_err(AppError::from)
}

/// Loads the index state for a previously processed file.
pub fn load_index_state(
    connection: &mut rusqlite::Connection,
    source_path: &str,
) -> Result<Option<SessionIndexState>, AppError> {
    connection
        .query_row(
            "SELECT source_path,
                    source,
                    file_size,
                    file_mtime,
                    last_parsed_byte_offset,
                    live_partial,
                    last_error
             FROM session_index_state
             WHERE source_path = ?1",
            [source_path],
            read_index_state,
        )
        .optional()
        .map_err(AppError::from)
}

/// Loads a session record by ID.
pub fn load_indexed_session(
    connection: &mut rusqlite::Connection,
    session_id: &str,
) -> Result<Option<IndexedSession>, AppError> {
    connection
        .query_row(
            "SELECT id,
                    source,
                    source_path,
                    source_session_id,
                    project_id,
                    cwd,
                    started_at,
                    ended_at,
                    duration_ms,
                    message_count,
                    tokens_in,
                    tokens_out,
                    model,
                    attribution_method,
                    index_error
             FROM sessions
             WHERE id = ?1",
            [session_id],
            read_indexed_session,
        )
        .optional()
        .map_err(AppError::from)
}

/// Persists the current index state for a session file.
pub fn save_index_state(
    transaction: &rusqlite::Transaction<'_>,
    state: &SessionIndexState,
    now: i64,
) -> Result<(), AppError> {
    transaction
        .execute(
            "INSERT INTO session_index_state (
                source_path,
                source,
                file_size,
                file_mtime,
                last_parsed_byte_offset,
                live_partial,
                last_error,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(source_path) DO UPDATE SET
                source = excluded.source,
                file_size = excluded.file_size,
                file_mtime = excluded.file_mtime,
                last_parsed_byte_offset = excluded.last_parsed_byte_offset,
                live_partial = excluded.live_partial,
                last_error = excluded.last_error,
                updated_at = excluded.updated_at",
            params![
                state.source_path,
                state.source.as_str(),
                state.file_size,
                state.file_mtime,
                state.last_parsed_byte_offset,
                state.live_partial,
                state.last_error,
                now,
            ],
        )
        .map(|_| ())
        .map_err(AppError::from)
}

/// Replaces all sessions for a file and saves index state.
pub fn persist_indexed_file_result(
    connection: &mut rusqlite::Connection,
    sessions: &[IndexedSession],
    state: &SessionIndexState,
    now: i64,
) -> Result<(), AppError> {
    with_write_txn(connection, |transaction| {
        transaction
            .execute(
                "DELETE FROM sessions WHERE source_path = ?1",
                [&state.source_path],
            )
            .map_err(AppError::from)?;
        for session in sessions {
            upsert_indexed_session(transaction, session, now)?;
        }
        save_index_state(transaction, state, now)
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionIndexClearSummary {
    pub sessions_cleared: i64,
    pub index_states_cleared: i64,
}

/// Removes all sessions and index states from the database.
pub fn clear_session_index(
    connection: &mut rusqlite::Connection,
) -> Result<SessionIndexClearSummary, AppError> {
    with_write_txn(connection, |transaction| {
        clear_session_index_in_transaction(transaction)
    })
}

/// Clears sessions and index states within a transaction.
pub fn clear_session_index_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
) -> Result<SessionIndexClearSummary, AppError> {
    let sessions_cleared = transaction
        .execute("DELETE FROM sessions", [])
        .map_err(AppError::from)?;
    let index_states_cleared = transaction
        .execute("DELETE FROM session_index_state", [])
        .map_err(AppError::from)?;

    Ok(SessionIndexClearSummary {
        sessions_cleared: sessions_cleared as i64,
        index_states_cleared: index_states_cleared as i64,
    })
}

/// Deletes all sessions without a project attribution.
pub fn prune_unmatched_sessions(connection: &mut rusqlite::Connection) -> Result<i64, AppError> {
    execute_delete(connection, "DELETE FROM sessions WHERE project_id IS NULL")
}

/// Removes index states with no corresponding sessions.
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

/// Removes index states for Codex sessions lacking token data.
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

/// Deletes sessions and index states under a path prefix.
pub fn prune_indexed_paths_under(
    connection: &mut rusqlite::Connection,
    path_prefix: &str,
) -> Result<i64, AppError> {
    with_write_txn(connection, |transaction| {
        let path_prefix_with_separator = format!("{}/", path_prefix.trim_end_matches('/'));
        let prefix_len: i64 = path_prefix_with_separator
            .len()
            .try_into()
            .map_err(|_| AppError::store("session path prefix is too long"))?;
        let pruned_sessions = transaction
            .execute(
                "DELETE FROM sessions
             WHERE source_path = ?1 OR substr(source_path, 1, ?2) = ?3",
                params![path_prefix, prefix_len, path_prefix_with_separator],
            )
            .map_err(AppError::from)?;
        transaction
            .execute(
                "DELETE FROM session_index_state
             WHERE source_path = ?1 OR substr(source_path, 1, ?2) = ?3",
                params![path_prefix, prefix_len, path_prefix_with_separator],
            )
            .map_err(AppError::from)?;

        Ok(pruned_sessions as i64)
    })
}

/// Re-attributes unmatched sessions to known projects.
pub fn rematch_unmatched_sessions_against_projects(
    connection: &mut rusqlite::Connection,
    known_projects: &[ProjectRoot],
    now: i64,
) -> Result<i64, AppError> {
    let unmatched_sessions = load_unmatched_sessions(connection)?;
    with_write_txn(connection, |transaction| {
        let mut rematched_count = 0;

        for session in unmatched_sessions {
            if let Some((project_id, method)) = match_project(&session, known_projects) {
                transaction
                    .execute(
                        "UPDATE sessions
                     SET project_id = ?1,
                         attribution_method = ?2,
                         updated_at = ?3
                     WHERE id = ?4 AND project_id IS NULL",
                        params![project_id, method, now, session.id],
                    )
                    .map_err(AppError::from)?;
                rematched_count += 1;
            }
        }

        Ok(rematched_count)
    })
}

/// Loads today's session counts, tokens, and 7-day sparklines.
pub fn load_portfolio_session_summary(
    connection: &mut rusqlite::Connection,
    visible_project_ids: &[String],
    today_start_ms: i64,
    seven_days_start_ms: i64,
) -> Result<PortfolioSessionSummary, AppError> {
    let visible_project_ids = visible_project_ids.iter().cloned().collect::<HashSet<_>>();
    let mut sparkline_by_project = visible_project_ids
        .iter()
        .map(|project_id| (project_id.clone(), [0; 7]))
        .collect::<BTreeMap<_, _>>();
    let mut sessions_today = 0;
    let mut tokens_today = 0;

    {
        let mut statement = connection
            .prepare(
                "SELECT project_id, started_at, tokens_in, tokens_out
                 FROM sessions
                 WHERE started_at >= ?1",
            )
            .map_err(AppError::from)?;
        let rows = statement
            .query_map([seven_days_start_ms], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<i64>>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                ))
            })
            .map_err(AppError::from)?;

        for row in rows {
            let (project_id, started_at, tokens_in, tokens_out) = row.map_err(AppError::from)?;
            let Some(started_at) = started_at else {
                continue;
            };

            if started_at >= today_start_ms {
                sessions_today += 1;
                tokens_today += tokens_in.unwrap_or(0) + tokens_out.unwrap_or(0);
            }

            let Some(project_id) = project_id else {
                continue;
            };
            if !visible_project_ids.contains(&project_id) {
                continue;
            }
            let bucket = (started_at - seven_days_start_ms) / DAY_MS;
            if (0..7).contains(&bucket) {
                if let Some(buckets) = sparkline_by_project.get_mut(&project_id) {
                    buckets[bucket as usize] += 1;
                }
            }
        }
    }

    let (unmatched_count, unmatched_claude_count, unmatched_codex_count) =
        load_unmatched_counts(connection)?;
    let recent_unmatched = load_recent_unmatched(connection)?;

    Ok(PortfolioSessionSummary {
        sessions_today,
        tokens_today,
        sparkline_by_project,
        unmatched_count,
        unmatched_claude_count,
        unmatched_codex_count,
        recent_unmatched,
    })
}

fn load_unmatched_sessions(
    connection: &mut rusqlite::Connection,
) -> Result<Vec<IndexedSession>, AppError> {
    let mut statement = connection
        .prepare(
            "SELECT id,
                    source,
                    source_path,
                    source_session_id,
                    project_id,
                    cwd,
                    started_at,
                    ended_at,
                    duration_ms,
                    message_count,
                    tokens_in,
                    tokens_out,
                    model,
                    attribution_method,
                    index_error
             FROM sessions
             WHERE project_id IS NULL",
        )
        .map_err(AppError::from)?;
    let rows = statement
        .query_map([], read_indexed_session)
        .map_err(AppError::from)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

fn load_unmatched_counts(
    connection: &mut rusqlite::Connection,
) -> Result<(i64, i64, i64), AppError> {
    connection
        .query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN source = 'claude' THEN 1 ELSE 0 END), 0),
                    COALESCE(SUM(CASE WHEN source = 'codex' THEN 1 ELSE 0 END), 0)
             FROM sessions
             WHERE project_id IS NULL",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(AppError::from)
}

fn load_recent_unmatched(
    connection: &mut rusqlite::Connection,
) -> Result<Vec<UnmatchedSessionSummary>, AppError> {
    let mut statement = connection
        .prepare(
            "SELECT id, source, source_path, cwd, started_at, model, index_error
             FROM sessions
             WHERE project_id IS NULL
             ORDER BY COALESCE(started_at, 0) DESC, updated_at DESC
             LIMIT ?1",
        )
        .map_err(AppError::from)?;
    let rows = statement
        .query_map([RECENT_UNMATCHED_LIMIT], |row| {
            let source: String = row.get(1)?;
            Ok(UnmatchedSessionSummary {
                id: row.get(0)?,
                source: SessionSource::try_from(source.as_str()).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        1,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
                source_path: row.get(2)?,
                cwd: row.get(3)?,
                started_at: row.get(4)?,
                model: row.get(5)?,
                index_error: row.get(6)?,
            })
        })
        .map_err(AppError::from)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

fn match_project(session: &IndexedSession, projects: &[ProjectRoot]) -> Option<(String, String)> {
    for project in projects {
        if let Some(cwd) = &session.cwd {
            if path_matches_root(cwd, &project.root_path) {
                return Some((project.id.clone(), "cwd".to_string()));
            }
        }

        if session.source == SessionSource::Claude
            && claude_source_path_matches_root(&session.source_path, &project.root_path)
        {
            return Some((project.id.clone(), "claude_path".to_string()));
        }
    }

    None
}

fn path_matches_root(path: &str, root: &str) -> bool {
    let root = root.trim_end_matches('/');
    path == root || path.starts_with(&format!("{root}/"))
}

fn claude_source_path_matches_root(source_path: &str, root: &str) -> bool {
    let encoded_root = root.replace('/', "-");
    source_path
        .split('/')
        .any(|segment| segment == encoded_root)
}

fn read_index_state(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionIndexState> {
    let source: String = row.get(1)?;
    Ok(SessionIndexState {
        source_path: row.get(0)?,
        source: SessionSource::try_from(source.as_str()).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        file_size: row.get(2)?,
        file_mtime: row.get(3)?,
        last_parsed_byte_offset: row.get(4)?,
        live_partial: row.get(5)?,
        last_error: row.get(6)?,
    })
}

fn read_indexed_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexedSession> {
    let source: String = row.get(1)?;
    Ok(IndexedSession {
        id: row.get(0)?,
        source: SessionSource::try_from(source.as_str()).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        source_path: row.get(2)?,
        source_session_id: row.get(3)?,
        project_id: row.get(4)?,
        cwd: row.get(5)?,
        started_at: row.get(6)?,
        ended_at: row.get(7)?,
        duration_ms: row.get(8)?,
        message_count: row.get(9)?,
        tokens_in: row.get(10)?,
        tokens_out: row.get(11)?,
        model: row.get(12)?,
        attribution_method: row.get(13)?,
        index_error: row.get(14)?,
    })
}
