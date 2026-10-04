//! 界面偏好读写路由。
//!
//! 服务端只存不解释：宽度区间与候选上限都是界面策略，这里不重复实现。

use astrcode_protocol::http::{UiPreferencesResponseDto, UpdateUiPreferencesRequest};
use astrcode_storage::ui_preferences::UiPreferences;
use axum::{
    Json,
    extract::State,
    response::{IntoResponse as _, Response},
};

use super::super::{HttpState, internal_error_response};

pub(in crate::http) async fn get_ui_preferences(State(state): State<HttpState>) -> Response {
    let loaded = match state.app.runtime().ui_preferences().load().await {
        Ok(loaded) => loaded,
        Err(error) => return internal_error_response("ui_preferences_read_failed", error),
    };
    Json(to_response_dto(loaded.stored, &loaded.preferences)).into_response()
}

pub(in crate::http) async fn update_ui_preferences(
    State(state): State<HttpState>,
    Json(request): Json<UpdateUiPreferencesRequest>,
) -> Response {
    let preferences = UiPreferences {
        sidebar_width: request.sidebar_width,
        collapsed_project_dirs: request.collapsed_project_dirs,
        kanban_project_paths: request.kanban_project_paths,
        kanban_ignored_project_paths: request.kanban_ignored_project_paths,
    };
    if let Err(error) = state
        .app
        .runtime()
        .ui_preferences()
        .save(&preferences)
        .await
    {
        return internal_error_response("ui_preferences_write_failed", error);
    }
    // 写入成功即算「已存过」，闸门只关心有没有写过，不关心写了什么。
    Json(to_response_dto(true, &preferences)).into_response()
}

fn to_response_dto(stored: bool, preferences: &UiPreferences) -> UiPreferencesResponseDto {
    UiPreferencesResponseDto {
        stored,
        sidebar_width: preferences.sidebar_width,
        collapsed_project_dirs: preferences.collapsed_project_dirs.clone(),
        kanban_project_paths: preferences.kanban_project_paths.clone(),
        kanban_ignored_project_paths: preferences.kanban_ignored_project_paths.clone(),
    }
}
