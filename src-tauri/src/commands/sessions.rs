use tauri::{ipc::Channel, State};

use crate::{
    app_state::AppState,
    error::AppError,
    events::SessionIndexEvent,
    sessions::{
        global::{self, GlobalChartDataDto, GlobalSessionsPageDto},
        indexer::{self, SessionIndexSummary},
        repo::{self, SessionIndexClearSummary},
    },
    store::daily_activity,
};

pub use crate::sessions::global::GlobalSessionFilters;

/// IPC command: indexes all session JSONL files and reports progress via channel.
#[tauri::command]
pub async fn index_sessions(
    state: State<'_, AppState>,
    on_event: Channel<SessionIndexEvent>,
) -> Result<SessionIndexSummary, AppError> {
    index_sessions_for_app(&state, move |event| {
        on_event.send(event).map_err(AppError::from)
    })
    .await
}

/// Indexes all session roots with a custom event callback.
pub async fn index_sessions_for_app(
    state: &AppState,
    on_event: impl Fn(SessionIndexEvent) -> Result<(), AppError> + Send + Sync + 'static,
) -> Result<SessionIndexSummary, AppError> {
    indexer::index_session_roots(state.pool.clone(), state.home_dir.clone(), on_event).await
}

/// IPC command: clears all indexed sessions and rebuilds daily activity.
#[tauri::command]
pub async fn clear_session_index(
    state: State<'_, AppState>,
) -> Result<SessionIndexClearSummary, AppError> {
    clear_session_index_for_app(&state).await
}

/// Clears the session index and rebuilds daily activity.
pub async fn clear_session_index_for_app(
    state: &AppState,
) -> Result<SessionIndexClearSummary, AppError> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(0))
        .unwrap_or(0);
    let connection = state.pool.get().await.map_err(AppError::store)?;
    let summary = connection
        .interact(move |connection| {
            crate::store::with_write_txn(connection, |transaction| {
                let summary = repo::clear_session_index_in_transaction(transaction)?;
                daily_activity::rebuild_window_in_transaction(transaction, 90, now_ms)?;
                Ok(summary)
            })
        })
        .await
        .map_err(AppError::store)??;

    Ok(summary)
}

/// IPC command: returns a paginated, filtered list of all sessions.
#[tauri::command]
pub async fn list_global_sessions(
    state: State<'_, AppState>,
    filters: GlobalSessionFilters,
    sort: Option<String>,
    direction: Option<String>,
    page: Option<i64>,
    page_size: Option<i64>,
) -> Result<GlobalSessionsPageDto, AppError> {
    list_global_sessions_for_app(
        &state,
        filters,
        sort.as_deref(),
        direction.as_deref(),
        page,
        page_size,
    )
    .await
}

/// IPC command: returns global chart data (by source, by project, histograms).
#[tauri::command]
pub async fn get_global_chart_data(
    state: State<'_, AppState>,
    filters: GlobalSessionFilters,
) -> Result<GlobalChartDataDto, AppError> {
    get_global_chart_data_for_app(&state, filters).await
}

/// Lists global sessions with filters, sorting, and pagination.
pub async fn list_global_sessions_for_app(
    state: &AppState,
    filters: GlobalSessionFilters,
    sort: Option<&str>,
    direction: Option<&str>,
    page: Option<i64>,
    page_size: Option<i64>,
) -> Result<GlobalSessionsPageDto, AppError> {
    let connection = state.pool.get().await.map_err(AppError::store)?;
    let sort = sort.map(str::to_string);
    let direction = direction.map(str::to_string);
    connection
        .interact(move |connection| {
            global::list_global_sessions(
                connection,
                &filters,
                sort.as_deref(),
                direction.as_deref(),
                page,
                page_size,
            )
        })
        .await
        .map_err(AppError::store)?
}

/// Loads global chart data with the given filters.
pub async fn get_global_chart_data_for_app(
    state: &AppState,
    filters: GlobalSessionFilters,
) -> Result<GlobalChartDataDto, AppError> {
    let connection = state.pool.get().await.map_err(AppError::store)?;
    connection
        .interact(move |connection| global::load_global_chart_data(connection, &filters))
        .await
        .map_err(AppError::store)?
}
