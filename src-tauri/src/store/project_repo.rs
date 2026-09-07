use rusqlite::{params, OptionalExtension};

use crate::error::AppError;
use crate::store::with_write_txn;

#[derive(Debug, Clone, PartialEq)]
pub struct StoredProjectSnapshot {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub planning_path: String,
    pub current_milestone_name: Option<String>,
    pub current_milestone_index: Option<i64>,
    pub current_phase_number: Option<String>,
    pub current_phase_name: Option<String>,
    pub milestone_progress_pct: f64,
    pub next_command: String,
    pub parsed_blob: String,
    pub parse_error: Option<String>,
    pub last_activity_at: Option<i64>,
    pub last_scanned_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPhasePlan {
    pub project_id: String,
    pub phase_number: String,
    pub phase_name: Option<String>,
    pub plan_number: Option<String>,
    pub plan_path: String,
    pub completed_at: Option<i64>,
    pub checklist_json: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlanItem {
    pub project_id: String,
    pub plan_path: String,
    pub ord: i64,
    pub text: String,
    pub checked: bool,
    pub line_no: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredScanLogEntry {
    pub project_id: Option<String>,
    pub root_path: Option<String>,
    pub planning_path: Option<String>,
    pub file_path: Option<String>,
    pub status: String,
    pub message: Option<String>,
    pub errors_json: String,
    pub created_at: i64,
}

/// Inserts or updates a project and its phase plans.
pub fn upsert_project_snapshot(
    connection: &mut rusqlite::Connection,
    snapshot: StoredProjectSnapshot,
    phase_plans: Vec<StoredPhasePlan>,
    now: i64,
) -> Result<(), AppError> {
    with_write_txn(connection, |transaction| {
    let project_id = snapshot.id.clone();
    let next_command = if snapshot.next_command.trim().is_empty() {
        "/gsd-next"
    } else {
        snapshot.next_command.as_str()
    };

    transaction
        .execute(
            "INSERT INTO projects (
                id,
                name,
                root_path,
                planning_path,
                current_milestone_name,
                current_milestone_index,
                current_phase_number,
                current_phase_name,
                milestone_progress_pct,
                next_command,
                parsed_blob,
                parse_error,
                last_activity_at,
                last_scanned_at,
                created_at,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                root_path = excluded.root_path,
                planning_path = excluded.planning_path,
                current_milestone_name = excluded.current_milestone_name,
                current_milestone_index = excluded.current_milestone_index,
                current_phase_number = excluded.current_phase_number,
                current_phase_name = excluded.current_phase_name,
                milestone_progress_pct = excluded.milestone_progress_pct,
                next_command = excluded.next_command,
                parsed_blob = excluded.parsed_blob,
                parse_error = excluded.parse_error,
                last_activity_at = excluded.last_activity_at,
                last_scanned_at = excluded.last_scanned_at,
                updated_at = excluded.updated_at",
            params![
                snapshot.id,
                snapshot.name,
                snapshot.root_path,
                snapshot.planning_path,
                snapshot.current_milestone_name,
                snapshot.current_milestone_index,
                snapshot.current_phase_number,
                snapshot.current_phase_name,
                snapshot.milestone_progress_pct,
                next_command,
                snapshot.parsed_blob,
                snapshot.parse_error,
                snapshot.last_activity_at,
                snapshot.last_scanned_at,
                now,
            ],
        )
        .map_err(AppError::from)?;

    transaction
        .execute(
            "DELETE FROM phase_plans WHERE project_id = ?1",
            [&project_id],
        )
        .map_err(AppError::from)?;

    for phase_plan in phase_plans {
        transaction
            .execute(
                "INSERT INTO phase_plans (
                    project_id,
                    phase_number,
                    phase_name,
                    plan_number,
                    plan_path,
                    completed_at,
                    checklist_json,
                    updated_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    phase_plan.project_id,
                    phase_plan.phase_number,
                    phase_plan.phase_name,
                    phase_plan.plan_number,
                    phase_plan.plan_path,
                    phase_plan.completed_at,
                    phase_plan.checklist_json,
                    now,
                ],
            )
            .map_err(AppError::from)?;
    }

    Ok(())
    })
}

/// Loads a project snapshot by its root path.
pub fn load_project_by_root(
    connection: &mut rusqlite::Connection,
    root_path: &str,
) -> Result<Option<StoredProjectSnapshot>, AppError> {
    connection
        .query_row(
            "SELECT id,
                    name,
                    root_path,
                    planning_path,
                    current_milestone_name,
                    current_milestone_index,
                    current_phase_number,
                    current_phase_name,
                    milestone_progress_pct,
                    next_command,
                    parsed_blob,
                    parse_error,
                    last_activity_at,
                    last_scanned_at,
                    created_at,
                    updated_at
             FROM projects
             WHERE root_path = ?1",
            [root_path],
            read_project_snapshot,
        )
        .optional()
        .map_err(AppError::from)
}

/// Loads a project snapshot by its ID.
pub fn load_project_by_id(
    connection: &mut rusqlite::Connection,
    project_id: &str,
) -> Result<Option<StoredProjectSnapshot>, AppError> {
    connection
        .query_row(
            "SELECT id,
                    name,
                    root_path,
                    planning_path,
                    current_milestone_name,
                    current_milestone_index,
                    current_phase_number,
                    current_phase_name,
                    milestone_progress_pct,
                    next_command,
                    parsed_blob,
                    parse_error,
                    last_activity_at,
                    last_scanned_at,
                    created_at,
                    updated_at
             FROM projects
             WHERE id = ?1",
            [project_id],
            read_project_snapshot,
        )
        .optional()
        .map_err(AppError::from)
}

/// Lists all projects ordered by recent activity.
pub fn list_project_snapshots(
    connection: &mut rusqlite::Connection,
) -> Result<Vec<StoredProjectSnapshot>, AppError> {
    let mut statement = connection
        .prepare(
            "SELECT id,
                    name,
                    root_path,
                    planning_path,
                    current_milestone_name,
                    current_milestone_index,
                    current_phase_number,
                    current_phase_name,
                    milestone_progress_pct,
                    next_command,
                    parsed_blob,
                    parse_error,
                    last_activity_at,
                    last_scanned_at,
                    created_at,
                    updated_at
             FROM projects
             ORDER BY COALESCE(last_activity_at, last_scanned_at) DESC,
                      name COLLATE NOCASE ASC",
        )
        .map_err(AppError::from)?;
    let rows = statement
        .query_map([], read_project_snapshot)
        .map_err(AppError::from)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

/// Deletes all project data from the cache.
pub fn clear_project_cache(connection: &mut rusqlite::Connection) -> Result<(), AppError> {
    with_write_txn(connection, |transaction| {
        transaction
            .execute("DELETE FROM phase_plans", [])
            .map_err(AppError::from)?;
        transaction
            .execute("DELETE FROM scan_log", [])
            .map_err(AppError::from)?;
        transaction
            .execute("DELETE FROM projects", [])
            .map_err(AppError::from)?;

        Ok(())
    })
}

/// Loads all phase plans for a project.
pub fn load_phase_plans(
    connection: &mut rusqlite::Connection,
    project_id: &str,
) -> Result<Vec<StoredPhasePlan>, AppError> {
    let mut statement = connection
        .prepare(
            "SELECT project_id,
                    phase_number,
                    phase_name,
                    plan_number,
                    plan_path,
                    completed_at,
                    checklist_json,
                    updated_at
             FROM phase_plans
             WHERE project_id = ?1
             ORDER BY phase_number, plan_number, plan_path",
        )
        .map_err(AppError::from)?;
    let rows = statement
        .query_map([project_id], read_phase_plan)
        .map_err(AppError::from)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

/// Replaces all plan items for a given plan path.
pub fn replace_plan_items(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    plan_path: &str,
    items: Vec<StoredPlanItem>,
) -> Result<(), AppError> {
    with_write_txn(connection, |transaction| {
        transaction
            .execute(
                "DELETE FROM plan_items WHERE project_id = ?1 AND plan_path = ?2",
                params![project_id, plan_path],
            )
            .map_err(AppError::from)?;

        for item in items {
            transaction
                .execute(
                    "INSERT INTO plan_items (
                    project_id,
                    plan_path,
                    ord,
                    text,
                    checked,
                    line_no
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        item.project_id,
                        item.plan_path,
                        item.ord,
                        item.text,
                        i64::from(item.checked),
                        item.line_no,
                    ],
                )
                .map_err(AppError::from)?;
        }

        Ok(())
    })
}

/// Loads plan items for a specific project and plan path.
pub fn load_plan_items(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    plan_path: &str,
) -> Result<Vec<StoredPlanItem>, AppError> {
    let mut statement = connection
        .prepare(
            "SELECT project_id,
                    plan_path,
                    ord,
                    text,
                    checked,
                    line_no
             FROM plan_items
             WHERE project_id = ?1 AND plan_path = ?2
             ORDER BY ord",
        )
        .map_err(AppError::from)?;
    let rows = statement
        .query_map(params![project_id, plan_path], read_plan_item)
        .map_err(AppError::from)?;

    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

/// Marks a plan as completed if all items are checked.
pub fn set_plan_completed_at_if_all_checked(
    connection: &mut rusqlite::Connection,
    project_id: &str,
    plan_path: &str,
    completed_at: i64,
) -> Result<(), AppError> {
    let (total, checked): (i64, i64) = connection
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(checked), 0)
             FROM plan_items
             WHERE project_id = ?1 AND plan_path = ?2",
            params![project_id, plan_path],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(AppError::from)?;
    let value = (total > 0 && total == checked).then_some(completed_at);
    if total == 0 {
        return Ok(());
    }

    connection
        .execute(
            "UPDATE phase_plans
             SET completed_at = ?3
             WHERE project_id = ?1 AND plan_path = ?2",
            params![project_id, plan_path, value],
        )
        .map(|_| ())
        .map_err(AppError::from)
}

/// Appends an entry to the scan log table.
pub fn append_scan_log(
    connection: &mut rusqlite::Connection,
    entry: StoredScanLogEntry,
    now: i64,
) -> Result<(), AppError> {
    connection
        .execute(
            "INSERT INTO scan_log (
                project_id,
                root_path,
                planning_path,
                file_path,
                status,
                message,
                errors_json,
                created_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                entry.project_id,
                entry.root_path,
                entry.planning_path,
                entry.file_path,
                entry.status,
                entry.message,
                entry.errors_json,
                now,
            ],
        )
        .map(|_| ())
        .map_err(AppError::from)
}

fn read_project_snapshot(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredProjectSnapshot> {
    Ok(StoredProjectSnapshot {
        id: row.get(0)?,
        name: row.get(1)?,
        root_path: row.get(2)?,
        planning_path: row.get(3)?,
        current_milestone_name: row.get(4)?,
        current_milestone_index: row.get(5)?,
        current_phase_number: row.get(6)?,
        current_phase_name: row.get(7)?,
        milestone_progress_pct: row.get(8)?,
        next_command: row.get(9)?,
        parsed_blob: row.get(10)?,
        parse_error: row.get(11)?,
        last_activity_at: row.get(12)?,
        last_scanned_at: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
    })
}

fn read_phase_plan(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredPhasePlan> {
    Ok(StoredPhasePlan {
        project_id: row.get(0)?,
        phase_number: row.get(1)?,
        phase_name: row.get(2)?,
        plan_number: row.get(3)?,
        plan_path: row.get(4)?,
        completed_at: row.get(5)?,
        checklist_json: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn read_plan_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredPlanItem> {
    let checked: i64 = row.get(4)?;
    Ok(StoredPlanItem {
        project_id: row.get(0)?,
        plan_path: row.get(1)?,
        ord: row.get(2)?,
        text: row.get(3)?,
        checked: checked != 0,
        line_no: row.get(5)?,
    })
}
