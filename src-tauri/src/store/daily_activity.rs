use rusqlite::params;
use std::collections::HashMap;

use crate::error::AppError;
use crate::store::with_write_txn;

const MIN_DAYS: i64 = 1;
const MAX_DAYS: i64 = 365;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyActivityRow {
    pub date: String,
    pub session_count: i64,
    pub token_total: i64,
    pub top_project_id: Option<String>,
    pub top_project_name: Option<String>,
}

/// Rebuilds the daily_activity table for a sliding window of days.
pub fn rebuild_window(
    connection: &mut rusqlite::Connection,
    days: i64,
    now_ms: i64,
) -> Result<(), AppError> {
    with_write_txn(connection, |transaction| {
        rebuild_window_in_transaction(transaction, days, now_ms)
    })
}

/// Rebuilds daily activity within an existing transaction.
pub fn rebuild_window_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    days: i64,
    now_ms: i64,
) -> Result<(), AppError> {
    let days = clamp_days(days);
    let today = local_date_for_ms_in_transaction(transaction, now_ms)?;
    let window_start = today - time::Duration::days(days - 1);
    let today_key = today.to_string();
    let window_start_key = window_start.to_string();
    let updated_at = now_ms / 1_000;

    transaction
        .execute(
            "DELETE FROM daily_activity
             WHERE date BETWEEN ?1 AND ?2",
            params![window_start_key.as_str(), today_key.as_str()],
        )
        .map_err(AppError::from)?;

    transaction
        .execute(
            "INSERT INTO daily_activity (date, session_count, token_total, top_project_id, updated_at)
             WITH session_days AS (
                 SELECT date(started_at / 1000, 'unixepoch', 'localtime') AS date,
                        project_id,
                        COALESCE(tokens_in, 0)
                            + COALESCE(tokens_out, 0)
                            + COALESCE(cache_read_tokens, 0)
                            + COALESCE(cache_creation_tokens, 0) AS token_total
                 FROM sessions
                 WHERE date(started_at / 1000, 'unixepoch', 'localtime') BETWEEN ?1 AND ?2
             ),
             day_totals AS (
                 SELECT date,
                        COUNT(*) AS session_count,
                        COALESCE(SUM(token_total), 0) AS token_total
                 FROM session_days
                 GROUP BY date
             ),
             project_rank AS (
                 SELECT date,
                        project_id,
                        COUNT(*) AS project_session_count,
                        ROW_NUMBER() OVER (
                            PARTITION BY date
                            ORDER BY COUNT(*) DESC, project_id ASC
                        ) AS row_number
                 FROM session_days
                 WHERE project_id IS NOT NULL
                 GROUP BY date, project_id
             )
             SELECT day_totals.date,
                    day_totals.session_count,
                    day_totals.token_total,
                    project_rank.project_id,
                    ?3
             FROM day_totals
             LEFT JOIN project_rank
               ON project_rank.date = day_totals.date
              AND project_rank.row_number = 1",
            params![window_start_key.as_str(), today_key.as_str(), updated_at],
        )
        .map_err(AppError::from)?;

    Ok(())
}

fn local_date_for_ms_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    now_ms: i64,
) -> Result<time::Date, AppError> {
    let date = transaction
        .query_row(
            "SELECT date(?1 / 1000, 'unixepoch', 'localtime')",
            [now_ms],
            |row| row.get::<_, String>(0),
        )
        .map_err(AppError::from)?;

    parse_date_key(&date)
}

/// Loads daily activity rows for the given number of days.
pub fn load_window(
    connection: &mut rusqlite::Connection,
    days: i64,
) -> Result<Vec<DailyActivityRow>, AppError> {
    let days = clamp_days(days);
    let end_date = latest_activity_date(connection)?.unwrap_or_else(current_local_date);
    let start_date = end_date - time::Duration::days(days - 1);
    let mut rows = Vec::with_capacity(days as usize);
    let start_key = start_date.to_string();
    let end_key = end_date.to_string();

    let mut statement = connection
        .prepare(
            "SELECT daily_activity.date,
                    daily_activity.session_count,
                    daily_activity.token_total,
                    daily_activity.top_project_id,
                    projects.name
             FROM daily_activity
             LEFT JOIN projects ON projects.id = daily_activity.top_project_id
             WHERE daily_activity.date BETWEEN ?1 AND ?2",
        )
        .map_err(AppError::from)?;
    let loaded_rows = statement
        .query_map(params![start_key, end_key], |row| {
            Ok(DailyActivityRow {
                date: row.get(0)?,
                session_count: row.get(1)?,
                token_total: row.get(2)?,
                top_project_id: row.get(3)?,
                top_project_name: row.get(4)?,
            })
        })
        .map_err(AppError::from)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(AppError::from)?;
    let mut rows_by_date = loaded_rows
        .into_iter()
        .map(|row| (row.date.clone(), row))
        .collect::<HashMap<_, _>>();

    for offset in 0..days {
        let date = start_date + time::Duration::days(offset);
        let date_key = date.to_string();

        rows.push(rows_by_date.remove(&date_key).unwrap_or(DailyActivityRow {
            date: date_key,
            session_count: 0,
            token_total: 0,
            top_project_id: None,
            top_project_name: None,
        }));
    }

    Ok(rows)
}

fn latest_activity_date(
    connection: &mut rusqlite::Connection,
) -> Result<Option<time::Date>, AppError> {
    let latest = connection
        .query_row("SELECT MAX(date) FROM daily_activity", [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .map_err(AppError::from)?;

    latest.map(|date| parse_date_key(&date)).transpose()
}

fn current_local_date() -> time::Date {
    time::OffsetDateTime::now_local()
        .unwrap_or_else(|error| {
            eprintln!("local date lookup failed, falling back to UTC date: {error}");
            time::OffsetDateTime::now_utc()
        })
        .date()
}

fn parse_date_key(date: &str) -> Result<time::Date, AppError> {
    let mut parts = date.split('-');
    let year = parts
        .next()
        .ok_or_else(|| AppError::store("missing year"))?
        .parse::<i32>()
        .map_err(AppError::store)?;
    let month = parts
        .next()
        .ok_or_else(|| AppError::store("missing month"))?
        .parse::<u8>()
        .map_err(AppError::store)?;
    let day = parts
        .next()
        .ok_or_else(|| AppError::store("missing day"))?
        .parse::<u8>()
        .map_err(AppError::store)?;
    if parts.next().is_some() {
        return Err(AppError::store("invalid date key"));
    }

    let month = time::Month::try_from(month).map_err(AppError::store)?;
    time::Date::from_calendar_date(year, month, day).map_err(AppError::store)
}

fn clamp_days(days: i64) -> i64 {
    days.clamp(MIN_DAYS, MAX_DAYS)
}
