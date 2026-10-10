//! 代码浏览路由：目录列举、文件读取、相对 git HEAD 的改动与整份工作区清单。
//!
//! 只有 wire 适配：参数从查询串进来，结果映射回 DTO。文件系统语义与路径校验都在
//! [`crate::file_browser`] 里，这一层不重复实现。

use std::path::PathBuf;

use astrcode_protocol::http::{
    FileContentResponseDto, FileDiffResponseDto, FileTreeResponseDto, GitStatusResponseDto,
};
use axum::{
    Json,
    extract::Query,
    response::{IntoResponse as _, Response},
};
use serde::Deserialize;

use super::super::{bad_request_response, internal_error_response, not_found_response};
use crate::file_browser::{self, FileBrowserError};

/// 三个浏览接口共用的查询参数。
#[derive(Debug, Deserialize)]
pub(in crate::http) struct FileQuery {
    /// 浏览根目录，由客户端给（项目工作目录）。
    root: String,
    /// 相对根目录的路径；缺省指根目录本身。
    #[serde(default)]
    path: String,
}

pub(in crate::http) async fn file_tree(Query(query): Query<FileQuery>) -> Response {
    let root = PathBuf::from(&query.root);
    match file_browser::list_dir(&root, &query.path).await {
        Ok(response) => Json::<FileTreeResponseDto>(response).into_response(),
        Err(error) => browser_error_response(error),
    }
}

pub(in crate::http) async fn file_content(Query(query): Query<FileQuery>) -> Response {
    let root = PathBuf::from(&query.root);
    match file_browser::read_file(&root, &query.path).await {
        Ok(response) => Json::<FileContentResponseDto>(response).into_response(),
        Err(error) => browser_error_response(error),
    }
}

pub(in crate::http) async fn file_diff(Query(query): Query<FileQuery>) -> Response {
    let root = PathBuf::from(&query.root);
    match file_browser::file_diff(&root, &query.path).await {
        Ok(response) => Json::<FileDiffResponseDto>(response).into_response(),
        Err(error) => browser_error_response(error),
    }
}

/// 工作区清单只要浏览根目录：它回答的是整个工作区里有哪些改动，与单个路径无关。
#[derive(Debug, Deserialize)]
pub(in crate::http) struct RootQuery {
    /// 浏览根目录，由客户端给（项目工作目录）。
    root: String,
}

pub(in crate::http) async fn file_status(Query(query): Query<RootQuery>) -> Response {
    let root = PathBuf::from(&query.root);
    match file_browser::worktree_status(&root).await {
        Ok(response) => Json::<GitStatusResponseDto>(response).into_response(),
        Err(error) => browser_error_response(error),
    }
}

/// 浏览失败的线缆映射：路径有问题回 400，目标不存在回 404，其余算服务端问题。
fn browser_error_response(error: FileBrowserError) -> Response {
    match error {
        FileBrowserError::InvalidPath(message) => bad_request_response("invalid_path", message),
        FileBrowserError::NotFound(message) => not_found_response("file_not_found", message),
        FileBrowserError::Io { path, source } => {
            internal_error_response("file_read_failed", format!("读取 {path} 失败：{source}"))
        },
    }
}
