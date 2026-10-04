//! 项目路径输入框的候选：最近用过的历史与「不想再看」的忽略集。
//!
//! 路径的权威来源始终是卡片本身与扩展配置，这里读不到历史只会少几个候选，
//! 不影响卡片创建。
//!
//! 候选由历史、默认目录与会话目录三处合并而来，因此「删除」除了清历史还要记一份
//! 已忽略路径：只清历史的话，会话目录推导出的候选下一轮渲染就回来了。
//!
//! 本模块是纯函数，落盘由调用方通过 UI 偏好（见 [`crate::preferences`]）完成。

/// 候选上限；超出后丢弃最久未使用的路径。
pub const PROJECT_PATH_HISTORY_LIMIT: usize = 10;

/// 忽略上限；超出后丢弃最早忽略的路径。
pub const IGNORED_PROJECT_PATH_LIMIT: usize = 50;

/// 归一化从服务端读回的路径列表：去首尾空白、丢弃空串，并截到上限。
///
/// 三项都在这里补齐而不是只靠写入侧：读取侧面对的是磁盘上的既有内容，可能被手工改过，
/// 也可能由更老的客户端写下。
pub fn normalize_path_list(paths: &[String], limit: usize) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.trim())
        .filter(|path| !path.is_empty())
        .take(limit)
        .map(str::to_string)
        .collect()
}

/// 把路径提到最前，同时撤销该路径的忽略。
///
/// 撤销忽略是因为用户又用它建了卡片，说明之前那次「删除」不再成立。
pub fn remember_project_path(
    history: &[String],
    ignored: &[String],
    working_dir: &str,
) -> (Vec<String>, Vec<String>) {
    let trimmed = working_dir.trim();
    if trimmed.is_empty() {
        return (history.to_vec(), ignored.to_vec());
    }

    let mut next = Vec::with_capacity(history.len() + 1);
    next.push(trimmed.to_string());
    next.extend(
        history
            .iter()
            .filter(|path| path.as_str() != trimmed)
            .cloned(),
    );
    next.truncate(PROJECT_PATH_HISTORY_LIMIT);

    let ignored = ignored
        .iter()
        .filter(|path| path.as_str() != trimmed)
        .cloned()
        .collect();
    (next, ignored)
}

/// 从候选中移除某条路径：清历史，并记进忽略集。
///
/// 只清历史不够——候选是历史、默认目录与会话目录的并集，否则推导出来的候选下一轮就回来。
/// 只影响候选列表：输入框里已填的草稿、以及空输入时的默认目录都不受影响。
/// 再次用它建卡片会撤销忽略，见 [`remember_project_path`]。
pub fn forget_project_path(
    history: &[String],
    ignored: &[String],
    working_dir: &str,
) -> (Vec<String>, Vec<String>) {
    let trimmed = working_dir.trim();
    if trimmed.is_empty() {
        return (history.to_vec(), ignored.to_vec());
    }

    let next = history
        .iter()
        .filter(|path| path.as_str() != trimmed)
        .cloned()
        .collect();

    let mut ignored_next = Vec::with_capacity(ignored.len() + 1);
    ignored_next.push(trimmed.to_string());
    ignored_next.extend(
        ignored
            .iter()
            .filter(|path| path.as_str() != trimmed)
            .cloned(),
    );
    ignored_next.truncate(IGNORED_PROJECT_PATH_LIMIT);

    (next, ignored_next)
}

/// 合并历史与其它来源的路径候选：历史顺序优先，去重、丢弃空值，并剔除已忽略的路径。
///
/// 忽略必须在这一层过滤，而不是靠调用方先删掉来源：被忽略的路径可能正是当前会话目录。
pub fn merge_project_path_candidates(
    history: &[String],
    sources: &[&str],
    ignored: &[String],
) -> Vec<String> {
    let ignored: Vec<&str> = ignored.iter().map(|path| path.trim()).collect();
    let mut seen = std::collections::HashSet::new();
    let mut candidates = Vec::new();
    for source in history
        .iter()
        .map(String::as_str)
        .chain(sources.iter().copied())
    {
        let trimmed = source.trim();
        if trimmed.is_empty() || ignored.contains(&trimmed) || !seen.insert(trimmed) {
            continue;
        }
        candidates.push(trimmed.to_string());
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn normalize_trims_drops_empties_and_clamps_to_the_limit() {
        let raw = paths(&[" /w/a ", "", "   ", "/w/b"]);

        assert_eq!(normalize_path_list(&raw, 10), paths(&["/w/a", "/w/b"]));
        assert_eq!(normalize_path_list(&raw, 1), paths(&["/w/a"]));
    }

    /// 上限只该由读取侧兜住：磁盘上可能已经有超出上限的列表。
    #[test]
    fn normalize_clamps_a_hand_edited_oversized_list() {
        let oversized: Vec<String> = (0..25).map(|index| format!("/w/{index}")).collect();

        let normalized = normalize_path_list(&oversized, PROJECT_PATH_HISTORY_LIMIT);

        assert_eq!(normalized.len(), PROJECT_PATH_HISTORY_LIMIT);
        assert_eq!(normalized[0], "/w/0", "截断必须保留最近使用的顺序");
    }

    #[test]
    fn remember_promotes_to_the_front_deduplicates_and_un_ignores() {
        let history = paths(&["/w/a", "/w/b"]);
        let ignored = paths(&["/w/b", "/w/c"]);

        let (next, ignored) = remember_project_path(&history, &ignored, "  /w/b ");

        assert_eq!(next, paths(&["/w/b", "/w/a"]));
        assert_eq!(ignored, paths(&["/w/c"]), "再次使用要撤销忽略");
    }

    #[test]
    fn remember_truncates_the_oldest_entry_at_the_limit() {
        let history: Vec<String> = (0..PROJECT_PATH_HISTORY_LIMIT)
            .map(|index| format!("/w/{index}"))
            .collect();

        let (next, _) = remember_project_path(&history, &[], "/w/new");

        assert_eq!(next.len(), PROJECT_PATH_HISTORY_LIMIT);
        assert_eq!(next[0], "/w/new");
        assert_eq!(next[PROJECT_PATH_HISTORY_LIMIT - 1], "/w/8");
        assert!(
            !next.contains(&"/w/9".to_string()),
            "最久未使用的必须被丢弃"
        );
    }

    #[test]
    fn forget_clears_the_history_and_records_the_ignore() {
        let history = paths(&["/w/a", "/w/b"]);
        let ignored = paths(&["/w/c"]);

        let (next, ignored) = forget_project_path(&history, &ignored, "/w/a");

        assert_eq!(next, paths(&["/w/b"]));
        assert_eq!(ignored, paths(&["/w/a", "/w/c"]));
    }

    #[test]
    fn forget_truncates_the_earliest_ignore_at_the_limit() {
        let ignored: Vec<String> = (0..IGNORED_PROJECT_PATH_LIMIT)
            .map(|index| format!("/w/{index}"))
            .collect();

        let (_, next) = forget_project_path(&[], &ignored, "/w/new");

        assert_eq!(next.len(), IGNORED_PROJECT_PATH_LIMIT);
        assert_eq!(next[0], "/w/new");
        assert!(!next.contains(&format!("/w/{}", IGNORED_PROJECT_PATH_LIMIT - 1)));
    }

    /// 空输入返回原值：草稿为空时不该把历史或忽略集清掉。
    #[test]
    fn an_empty_path_leaves_both_lists_untouched() {
        let history = paths(&["/w/a"]);
        let ignored = paths(&["/w/b"]);

        assert_eq!(
            remember_project_path(&history, &ignored, "   "),
            (history.clone(), ignored.clone())
        );
        assert_eq!(
            forget_project_path(&history, &ignored, ""),
            (history.clone(), ignored.clone())
        );
    }

    #[test]
    fn merge_keeps_history_order_and_drops_duplicates_and_blanks() {
        let history = paths(&["/w/a", "/w/b"]);

        let merged =
            merge_project_path_candidates(&history, &["/w/b", "", "  ", "/w/c", "/w/a"], &[]);

        assert_eq!(merged, paths(&["/w/a", "/w/b", "/w/c"]));
    }

    /// 忽略要在合并这一层过滤：被忽略的路径可能正是当前会话目录。
    #[test]
    fn merge_filters_ignored_paths_from_every_source() {
        let history = paths(&["/w/a"]);
        let ignored = paths(&[" /w/a ", "/w/session"]);

        let merged = merge_project_path_candidates(&history, &["/w/session", "/w/d"], &ignored);

        assert_eq!(merged, paths(&["/w/d"]));
    }
}
