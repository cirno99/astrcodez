//! 冷会话列举的摘要 sidecar 缓存。
//!
//! 列举会话时,没在本进程打开过的会话必须扫完整条事件日志才能算出摘要
//! (见 [`super::reader::read_summaries_from_logs`])。摘要本身很小,扫日志却很贵:
//! 成本与条目数×日志体积成正比,而侧边栏会因建/删/分叉等动作反复刷新。
//!
//! 缓存只用「日志长度」做有效性凭证:落盘时记录生成摘要时**实际解析**的字节数,
//! 命中要求日志当前长度与之完全相等。于是缓存不可能覆盖任何未解析的字节,
//! 日志一旦追加(长度变大)或被崩后恢复截断(长度变小),即自动回退到全量扫描。

use std::path::{Path, PathBuf};

use astrcode_core::types::SessionId;
use astrcode_session_projection::SessionSummary;
use serde::{Deserialize, Serialize};

/// sidecar 落盘格式版本。字段语义变化时递增,旧缓存按未命中处理。
const SUMMARY_CACHE_VERSION: u32 = 1;

/// sidecar 文件名。放在会话目录内,会话被删除或回收时随之消失。
const SUMMARY_CACHE_FILE: &str = "summary-cache.json";

/// 落盘的摘要缓存条目。
#[derive(Debug, Serialize, Deserialize)]
struct SummaryCacheEntry {
    version: u32,
    covered_len: u64,
    summary: SessionSummary,
}

fn cache_path(session_dir: &Path) -> PathBuf {
    session_dir.join(SUMMARY_CACHE_FILE)
}

/// 事件日志当前长度;日志不存在时返回 `None`。
pub(super) async fn log_len(log_path: &Path) -> Option<u64> {
    tokio::fs::metadata(log_path)
        .await
        .ok()
        .map(|meta| meta.len())
}

/// 读取覆盖长度与 `log_len` 相等的缓存摘要。
///
/// 未命中、损坏、版本不符、会话不符一律返回 `None`,由调用方回退全量扫描;这些值都来自
/// 磁盘,不能假定与请求的会话一致,因此逐个复核后才可用。
pub(super) async fn load(
    session_dir: &Path,
    session_id: &SessionId,
    log_len: u64,
) -> Option<SessionSummary> {
    let bytes = tokio::fs::read(cache_path(session_dir)).await.ok()?;
    let entry: SummaryCacheEntry = serde_json::from_slice(&bytes).ok()?;
    (entry.version == SUMMARY_CACHE_VERSION
        && entry.covered_len == log_len
        && entry.summary.session_id == *session_id)
        .then_some(entry.summary)
}

/// best-effort 回填缓存。写失败或写坏只影响下次列举的回退,不影响本次结果——
/// 读者遇到无法解析的缓存会按未命中处理。
pub(super) async fn store(session_dir: &Path, summary: &SessionSummary, covered_len: u64) {
    let entry = SummaryCacheEntry {
        version: SUMMARY_CACHE_VERSION,
        covered_len,
        summary: summary.clone(),
    };
    let bytes = match serde_json::to_vec(&entry) {
        Ok(bytes) => bytes,
        // 摘要只由字符串、可选字符串和标量枚举组成,编码不会失败;真失败也只跳过回填。
        Err(error) => {
            tracing::debug!(%error, "session summary cache encode failed");
            return;
        },
    };
    if let Err(error) = tokio::fs::write(cache_path(session_dir), bytes).await {
        tracing::debug!(
            path = %session_dir.display(),
            %error,
            "session summary cache write failed",
        );
    }
}
