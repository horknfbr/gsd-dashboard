use std::{path::Path, time::Duration};

use deadpool_sqlite::{Config, Hook, HookError, Pool, PoolConfig, Runtime};
use rusqlite::Connection;

use crate::error::AppError;

pub mod daily_activity;
pub mod migrations;
pub mod project_repo;
pub mod settings_repo;

/// Opens a deadpool SQLite connection pool with WAL mode.
pub async fn open_pool(db_path: &Path) -> Result<Pool, AppError> {
    let mut config = Config::new(db_path);
    config.pool = Some(PoolConfig::new(4));
    let pool = config
        .builder(Runtime::Tokio1)
        .map_err(AppError::store)?
        .post_create(Hook::async_fn(|connection, _| {
            Box::pin(async move {
                connection
                    .interact(configure_connection)
                    .await
                    .map_err(|error| HookError::message(error.to_string()))?
                    .map_err(HookError::Backend)
            })
        }))
        .build()
        .map_err(AppError::store)?;

    Ok(pool)
}

/// Applies pending schema migrations to the database.
pub async fn run_migrations(pool: &Pool) -> Result<(), AppError> {
    let connection = pool.get().await.map_err(AppError::store)?;
    connection
        .interact(migrations::run)
        .await
        .map_err(AppError::store)?
        .map_err(AppError::from)
}

/// Returns the current migration version number.
pub async fn migration_version(pool: &Pool) -> Result<u32, AppError> {
    let connection = pool.get().await.map_err(AppError::store)?;
    let version = connection
        .interact(|connection| {
            connection.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
        })
        .await
        .map_err(AppError::store)??;

    Ok(version)
}

/// Checks whether WAL journal mode is active.
pub async fn wal_enabled(pool: &Pool) -> Result<bool, AppError> {
    let connection = pool.get().await.map_err(AppError::store)?;
    let journal_mode = connection
        .interact(|connection| {
            connection.pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
        })
        .await
        .map_err(AppError::store)??;

    Ok(journal_mode.eq_ignore_ascii_case("wal"))
}

fn configure_connection(connection: &mut Connection) -> rusqlite::Result<()> {
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

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
