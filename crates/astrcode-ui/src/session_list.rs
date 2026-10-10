//! 侧边栏会话列表的分组与排序。
//!
//! 口径照搬前端的 `Sidebar.tsx` 与 `projectFolderOrder.ts`：项目顺序一旦定下就不再重排，
//! 组内按最近使用降序，折叠集合按当前项目剪枝。不依赖 gpui，可脱窗口测试。

use std::collections::{HashMap, HashSet, hash_map::Entry};

use astrcode_protocol::http::SessionListItemDto;

/// 会话还没有内容时行上写的占位。
pub(crate) const NEW_SESSION_LABEL: &str = "新对话";

/// 一个项目分组：工作目录，以及组内会话在 `sessions` 里的下标。
///
/// 用下标而不是克隆 DTO：列表的持有者是侧边栏，这里只回答「谁和谁一组、谁在组内排前面」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectGroup {
    pub working_dir: String,
    pub session_indices: Vec<usize>,
}

/// 工作目录的末段；路径里没有可用段时返回 `None`。
///
/// 分隔符按两种平台都认：会话的工作目录可能来自另一台机器。
pub(crate) fn project_name_tail(working_dir: &str) -> Option<&str> {
    working_dir
        .split(['/', '\\'])
        .rfind(|part| !part.is_empty())
}

/// 项目分组标题用名：没有可用段时整份退回（前端 `projectNameFromDir`）。
pub(crate) fn project_name(working_dir: &str) -> &str {
    project_name_tail(working_dir).unwrap_or(working_dir)
}

/// 把一段文本压成一行：所有空白（含换行、制表符）折叠成单个空格，首尾去掉。
///
/// 行高由文本最大行数决定，而 gpui 的 `shape_text` 会**先按 `\n` 切行再整形**，所以只挂
/// `.truncate()` 并不会把多行消息压成一行——一条带换行的首条用户消息能把会话行撑到好几行高。
/// 单行是这里的口径，因此归一化必须发生在渲染之前。
pub(crate) fn single_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 会话行上写什么：首条用户消息优先，其次标题，都没有时是占位。
///
/// 与 [`crate::views::display_title`] 的顺序**相反**——那是聊天顶栏的取法（标题优先），
/// 这里是侧边栏列表的取法，前端两处也是各写各的。
pub(crate) fn session_label(item: &SessionListItemDto) -> String {
    item.first_user_message
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .or_else(|| (!item.title.trim().is_empty()).then_some(item.title.as_str()))
        .map(single_line)
        .unwrap_or_else(|| NEW_SESSION_LABEL.to_owned())
}

/// 按查询串筛出会话：比的是行上写着的那串文字（首条用户消息或标题），大小写不敏感。
///
/// 空查询串给整份列表的副本：调用方拿到的那一份要拿去分组，组里的下标指的是它。
pub(crate) fn filter_sessions(
    sessions: &[SessionListItemDto],
    needle: &str,
) -> Vec<SessionListItemDto> {
    if needle.is_empty() {
        return sessions.to_vec();
    }
    sessions
        .iter()
        .filter(|item| {
            !crate::find::literal_matches(&session_label(item), needle, false).is_empty()
        })
        .cloned()
        .collect()
}

/// 按 `project_order` 给出分组：顺序里没有的目录按在列表里出现的先后追加在末尾。
///
/// 组内按最近使用降序（`updated_at`，相同则 `created_at`），与前端
/// `groupSessionsByWorkingDir` 加上 `orderedWorkingDirs` 合起来的结果一致。
pub(crate) fn group_sessions(
    sessions: &[SessionListItemDto],
    project_order: &[String],
) -> Vec<ProjectGroup> {
    let mut by_dir: HashMap<&str, Vec<usize>> = HashMap::new();
    let mut encountered: Vec<&str> = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        match by_dir.entry(session.working_dir.as_str()) {
            Entry::Occupied(mut entry) => entry.get_mut().push(index),
            Entry::Vacant(entry) => {
                encountered.push(session.working_dir.as_str());
                entry.insert(vec![index]);
            },
        }
    }

    let mut ordered: Vec<&str> = Vec::with_capacity(by_dir.len());
    for dir in project_order.iter().map(String::as_str) {
        if by_dir.contains_key(dir) && !ordered.contains(&dir) {
            ordered.push(dir);
        }
    }
    for dir in encountered {
        if !ordered.contains(&dir) {
            ordered.push(dir);
        }
    }

    ordered
        .into_iter()
        .map(|dir| {
            let mut session_indices = by_dir.remove(dir).unwrap_or_default();
            session_indices
                .sort_by(|left, right| compare_by_last_used(&sessions[*left], &sessions[*right]));
            ProjectGroup {
                working_dir: dir.to_owned(),
                session_indices,
            }
        })
        .collect()
}

/// 首次拿到的项目顺序：各组最早会话的 `created_at` 升序（前端
/// `computeInitialProjectFolderOrder`）。
///
/// 组与组的先后只在这里按时间定一次，之后一律走 [`sync_project_order`]，不再重排。
pub(crate) fn initial_project_order(sessions: &[SessionListItemDto]) -> Vec<String> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    // (工作目录, 组内最早 createdAt)，按首次出现顺序排列。
    let mut groups: Vec<(&str, &str)> = Vec::new();
    for session in sessions {
        match seen.get(session.working_dir.as_str()).copied() {
            Some(index) => {
                if session.created_at.as_str() < groups[index].1 {
                    groups[index].1 = session.created_at.as_str();
                }
            },
            None => {
                seen.insert(session.working_dir.as_str(), groups.len());
                groups.push((session.working_dir.as_str(), session.created_at.as_str()));
            },
        }
    }
    // `sort_by` 是稳定排序，同刻的组保持首次出现的先来后到。
    groups.sort_by(|left, right| left.1.cmp(right.1));
    groups.into_iter().map(|(dir, _)| dir.to_owned()).collect()
}

/// 已有顺序保持不变：移除已经消失的目录，新目录追加到末尾（前端 `syncProjectFolderOrder`）。
pub(crate) fn sync_project_order(
    current: &[String],
    sessions: &[SessionListItemDto],
) -> Vec<String> {
    let active: HashSet<&str> = sessions
        .iter()
        .map(|session| session.working_dir.as_str())
        .collect();
    let mut next: Vec<String> = current
        .iter()
        .filter(|dir| active.contains(dir.as_str()))
        .cloned()
        .collect();
    for session in sessions {
        if !next.iter().any(|dir| dir == &session.working_dir) {
            next.push(session.working_dir.clone());
        }
    }
    next
}

/// 切换一个项目的折叠态。
pub(crate) fn toggle_collapsed(collapsed: &[String], working_dir: &str) -> Vec<String> {
    if collapsed.iter().any(|dir| dir == working_dir) {
        collapsed
            .iter()
            .filter(|dir| dir.as_str() != working_dir)
            .cloned()
            .collect()
    } else {
        let mut next = collapsed.to_vec();
        next.push(working_dir.to_owned());
        next
    }
}

/// 删除之后选中谁：当前会话还在就留着，否则退到第一条不在被删项目里的会话；
/// 没有可选的（列表空了，或剩下的全在被删项目里）就返回 `None`。
///
/// `removed_working_dir` 只在删项目时给出。它是防御性的重查：服务端返回的列表本就不该再
/// 含那个目录，但跨了一次 HTTP 的值不值得信，宁可退到「没有会话」也不去选一个已经删掉的。
pub(crate) fn pick_active_after_delete(
    sessions: &[SessionListItemDto],
    active: Option<&str>,
    removed_working_dir: Option<&str>,
) -> Option<String> {
    let in_removed_dir =
        |item: &SessionListItemDto| removed_working_dir == Some(item.working_dir.as_str());
    if let Some(active) = active
        && let Some(item) = sessions
            .iter()
            .find(|item| item.session_id == active && !in_removed_dir(item))
    {
        return Some(item.session_id.clone());
    }
    sessions
        .iter()
        .find(|item| !in_removed_dir(item))
        .map(|item| item.session_id.clone())
}

/// 折叠集合里只留当前还在列表里的目录（前端 `visibleCollapsedProjectDirs`）。
///
/// 这一份是要落盘的：删掉的项目不能一直占着偏好文件，否则重建同名目录时会「自己折叠起来」。
pub(crate) fn prune_collapsed(collapsed: &[String], groups: &[ProjectGroup]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    collapsed
        .iter()
        .filter(|dir| groups.iter().any(|group| &group.working_dir == *dir))
        .filter(|dir| seen.insert(dir.as_str()))
        .cloned()
        .collect()
}

/// 选择态里仍然存在的会话 id。
///
/// 列表换了一批之后，陈旧 id 不能拿去删除（前端 `effectiveSelectedSessionIds` 同判据）；
/// 勾选顺序留着——同一批删除对这些 id 一视同仁。
pub(crate) fn prune_selected(selected: &[String], sessions: &[SessionListItemDto]) -> Vec<String> {
    selected
        .iter()
        .filter(|id| sessions.iter().any(|item| &item.session_id == *id))
        .cloned()
        .collect()
}

/// 勾选或取消勾选一条会话。
pub(crate) fn toggle_selected(selected: &[String], session_id: &str) -> Vec<String> {
    if selected.iter().any(|id| id == session_id) {
        selected
            .iter()
            .filter(|id| id.as_str() != session_id)
            .cloned()
            .collect()
    } else {
        let mut next = selected.to_vec();
        next.push(session_id.to_owned());
        next
    }
}

/// 是否每一条会话都被勾上。空列表为假：没有会话时不给「全选」这个状态。
pub(crate) fn all_selected(selected: &[String], sessions: &[SessionListItemDto]) -> bool {
    !sessions.is_empty()
        && sessions
            .iter()
            .all(|item| selected.iter().any(|id| id == &item.session_id))
}

/// 全选 / 取消全选：已经全都勾上就清空，否则补齐到全部。
pub(crate) fn toggle_select_all(
    selected: &[String],
    sessions: &[SessionListItemDto],
) -> Vec<String> {
    if all_selected(selected, sessions) {
        Vec::new()
    } else {
        sessions
            .iter()
            .map(|item| item.session_id.clone())
            .collect()
    }
}

/// 最近使用的排前面：`updated_at` 降序，相同则 `created_at` 降序。
fn compare_by_last_used(
    left: &SessionListItemDto,
    right: &SessionListItemDto,
) -> std::cmp::Ordering {
    right
        .updated_at
        .cmp(&left.updated_at)
        .then_with(|| right.created_at.cmp(&left.created_at))
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::wire::PhaseDto;

    use super::*;

    fn session(id: &str, dir: &str, created: &str, updated: &str) -> SessionListItemDto {
        SessionListItemDto {
            session_id: id.to_owned(),
            working_dir: dir.to_owned(),
            title: String::new(),
            created_at: created.to_owned(),
            updated_at: updated.to_owned(),
            phase: PhaseDto::Idle,
            first_user_message: None,
        }
    }

    fn dirs(groups: &[ProjectGroup]) -> Vec<&str> {
        groups
            .iter()
            .map(|group| group.working_dir.as_str())
            .collect()
    }

    fn ids<'a>(sessions: &'a [SessionListItemDto], group: &ProjectGroup) -> Vec<&'a str> {
        group
            .session_indices
            .iter()
            .map(|index| sessions[*index].session_id.as_str())
            .collect()
    }

    #[test]
    fn project_names_are_the_last_path_segment() {
        assert_eq!(project_name("/w/alpha"), "alpha");
        assert_eq!(project_name("/w/alpha/"), "alpha");
        assert_eq!(project_name("alpha"), "alpha");
        assert_eq!(project_name(r"C:\w\beta"), "beta");
        assert_eq!(project_name_tail("/w/alpha"), Some("alpha"));
        assert_eq!(project_name_tail("/"), None);
        assert_eq!(project_name_tail(""), None);
        // 没有可用段时整份退回，标题栏才不会空着。
        assert_eq!(project_name(""), "");
    }

    #[test]
    fn session_labels_prefer_the_first_message_then_the_title() {
        let mut item = session("s1", "/w/a", "2026-01-01", "2026-01-01");
        assert_eq!(session_label(&item), NEW_SESSION_LABEL);
        item.title = "标题".to_owned();
        assert_eq!(session_label(&item), "标题");
        item.first_user_message = Some("首条消息".to_owned());
        assert_eq!(session_label(&item), "首条消息");
        // 空白内容不算内容，退回下一个候选。
        item.first_user_message = Some("   ".to_owned());
        assert_eq!(session_label(&item), "标题");
    }

    #[test]
    fn a_label_is_folded_onto_one_line() {
        // 首条消息里带换行：渲染前就要压成一行，否则会话行会被撑高（gpui 按 `\n` 切行）。
        let mut item = session("s1", "/w/a", "2026-01-01", "2026-01-01");
        item.first_user_message = Some("先看这段\n\n再看  下一段\t结尾".to_owned());
        assert_eq!(session_label(&item), "先看这段 再看 下一段 结尾");
        assert_eq!(single_line("  \n "), "");
    }

    #[test]
    fn filtering_matches_the_row_label_ignoring_case() {
        let mut asked = session("a1", "/w/alpha", "2026-01-01", "2026-01-01");
        asked.first_user_message = Some("修一下 Value 的解析".to_owned());
        let mut titled = session("b1", "/w/beta", "2026-01-01", "2026-01-01");
        titled.title = "重构侧边栏".to_owned();
        let fresh = session("c1", "/w/beta", "2026-01-01", "2026-01-01");
        let sessions = vec![asked, titled, fresh];

        let ids = |found: &[SessionListItemDto]| {
            found
                .iter()
                .map(|item| item.session_id.clone())
                .collect::<Vec<_>>()
        };

        // 空查询串给全部（顺序原样）。
        assert_eq!(ids(&filter_sessions(&sessions, "")), ["a1", "b1", "c1"]);
        // 大小写不敏感。
        assert_eq!(ids(&filter_sessions(&sessions, "value")), ["a1"]);
        assert_eq!(ids(&filter_sessions(&sessions, "侧边栏")), ["b1"]);
        // 比的是行上写着的那串文字，不是工作目录。
        assert!(filter_sessions(&sessions, "alpha").is_empty());
        // 还没内容的会话按占位名找得到。
        assert_eq!(ids(&filter_sessions(&sessions, NEW_SESSION_LABEL)), ["c1"]);
        assert!(filter_sessions(&sessions, "没有这个").is_empty());
    }


    #[test]
    fn groups_follow_the_stored_order_and_append_unknown_dirs() {
        let sessions = vec![
            session("a1", "/w/alpha", "2026-01-01", "2026-01-03"),
            session("b1", "/w/beta", "2026-01-02", "2026-01-02"),
            session("a2", "/w/alpha", "2026-01-04", "2026-01-05"),
            session("c1", "/w/gamma", "2026-01-06", "2026-01-06"),
        ];
        // 顺序里只认得 beta；alpha 与 gamma 按首次出现顺序补在后面。
        let order = vec!["/w/beta".to_owned()];
        let groups = group_sessions(&sessions, &order);

        assert_eq!(dirs(&groups), ["/w/beta", "/w/alpha", "/w/gamma"]);
        // 组内最近使用在前。
        assert_eq!(ids(&sessions, &groups[1]), ["a2", "a1"]);
        assert_eq!(ids(&sessions, &groups[0]), ["b1"]);
    }

    #[test]
    fn groups_ignore_order_entries_that_no_longer_exist() {
        let sessions = vec![session("a1", "/w/alpha", "2026-01-01", "2026-01-01")];
        let order = vec!["/w/gone".to_owned(), "/w/alpha".to_owned()];
        let groups = group_sessions(&sessions, &order);
        assert_eq!(dirs(&groups), ["/w/alpha"]);
    }

    #[test]
    fn group_order_breaks_ties_on_created_at() {
        let sessions = vec![
            session("old", "/w/old", "2026-01-01", "2026-01-01"),
            session("new", "/w/new", "2026-02-01", "2026-02-01"),
        ];
        let groups = group_sessions(&sessions, &[]);
        assert_eq!(dirs(&groups), ["/w/old", "/w/new"]);
    }

    #[test]
    fn initial_order_is_by_each_groups_earliest_session() {
        let sessions = vec![
            // beta 的最早会话比 alpha 晚，即便它的最新会话更早出现。
            session("b1", "/w/beta", "2026-01-05", "2026-01-05"),
            session("a1", "/w/alpha", "2026-01-02", "2026-01-02"),
            session("b2", "/w/beta", "2026-01-01", "2026-01-09"),
        ];
        assert_eq!(initial_project_order(&sessions), ["/w/beta", "/w/alpha"]);
        assert!(initial_project_order(&[]).is_empty());
    }

    #[test]
    fn sync_keeps_the_order_and_appends_new_dirs_at_the_end() {
        let current = vec!["/w/beta".to_owned(), "/w/gone".to_owned()];
        let sessions = vec![
            session("g1", "/w/gamma", "2026-01-01", "2026-01-01"),
            session("b1", "/w/beta", "2026-01-01", "2026-01-01"),
        ];
        // beta 留在原位，gone 被移除，gamma 追加到末尾——不因为它的会话更靠前就插队。
        assert_eq!(
            sync_project_order(&current, &sessions),
            ["/w/beta", "/w/gamma"]
        );
        assert!(sync_project_order(&[], &[]).is_empty());
    }

    #[test]
    fn after_a_delete_the_current_session_is_kept_or_the_first_one_takes_over() {
        let sessions = vec![
            session("a1", "/w/alpha", "2026-01-01", "2026-01-01"),
            session("b1", "/w/beta", "2026-01-02", "2026-01-02"),
        ];

        // 删的不是当前会话：留着当前这个。
        assert_eq!(
            pick_active_after_delete(&sessions, Some("b1"), None).as_deref(),
            Some("b1")
        );
        // 删掉的正是当前会话（列表里已经没有它）：退到第一条。
        let rest = vec![session("b1", "/w/beta", "2026-01-02", "2026-01-02")];
        assert_eq!(
            pick_active_after_delete(&rest, Some("a1"), None).as_deref(),
            Some("b1")
        );
        // 列表空了就没有可选的。
        assert_eq!(pick_active_after_delete(&[], Some("a1"), None), None);
    }

    #[test]
    fn deleting_a_project_never_lands_on_a_session_that_is_gone() {
        let sessions = vec![
            session("a1", "/w/alpha", "2026-01-01", "2026-01-01"),
            session("b1", "/w/beta", "2026-01-02", "2026-01-02"),
        ];

        // 删掉 alpha 项目，当前会话正是 alpha 里的那个：换到 beta。
        assert_eq!(
            pick_active_after_delete(&sessions, Some("a1"), Some("/w/alpha")).as_deref(),
            Some("b1")
        );
        // 整个列表都在被删的项目里：没有可选的。
        let only_alpha = vec![session("a1", "/w/alpha", "2026-01-01", "2026-01-01")];
        assert_eq!(
            pick_active_after_delete(&only_alpha, Some("a1"), Some("/w/alpha")),
            None
        );
    }

    #[test]
    fn collapsing_is_a_toggle_and_survives_pruning() {
        let collapsed = toggle_collapsed(&[], "/w/alpha");
        assert_eq!(collapsed, ["/w/alpha"]);
        assert!(toggle_collapsed(&collapsed, "/w/alpha").is_empty());

        let sessions = vec![
            session("a1", "/w/alpha", "2026-01-01", "2026-01-01"),
            session("b1", "/w/beta", "2026-01-01", "2026-01-01"),
        ];
        let groups = group_sessions(&sessions, &[]);
        let kept = prune_collapsed(&["/w/gone".to_owned(), "/w/alpha".to_owned()], &groups);
        assert_eq!(kept, ["/w/alpha"]);
        // 重复项只留一份：折叠集合是集合语义。
        let deduped = prune_collapsed(&["/w/alpha".to_owned(), "/w/alpha".to_owned()], &groups);
        assert_eq!(deduped, ["/w/alpha"]);
    }

    #[test]
    fn selection_drops_ids_that_left_the_list() {
        let sessions = vec![
            session("a1", "/w/alpha", "2026-01-01", "2026-01-01"),
            session("b1", "/w/beta", "2026-01-01", "2026-01-01"),
        ];
        let selected = vec!["gone".to_owned(), "b1".to_owned(), "a1".to_owned()];

        assert_eq!(prune_selected(&selected, &sessions), ["b1", "a1"]);
        assert!(prune_selected(&["gone".to_owned()], &sessions).is_empty());
    }

    #[test]
    fn selection_is_a_toggle_per_session() {
        let one = toggle_selected(&[], "a1");
        assert_eq!(one, ["a1"]);
        assert!(toggle_selected(&one, "a1").is_empty());

        // 取消中间那一条不影响其余，也不重排。
        let two = toggle_selected(&one, "b1");
        assert_eq!(two, ["a1", "b1"]);
        assert_eq!(toggle_selected(&two, "a1"), ["b1"]);
    }

    #[test]
    fn select_all_needs_a_non_empty_list_and_flips_both_ways() {
        let sessions = vec![
            session("a1", "/w/alpha", "2026-01-01", "2026-01-01"),
            session("b1", "/w/beta", "2026-01-01", "2026-01-01"),
        ];

        assert!(!all_selected(&[], &[]), "空列表没有全选");
        let all = toggle_select_all(&[], &sessions);
        assert_eq!(all, ["a1", "b1"]);
        assert!(all_selected(&all, &sessions));
        assert!(
            toggle_select_all(&all, &sessions).is_empty(),
            "再点一次取消全选"
        );
        // 只勾了一部分时「全选」把其余的补齐。
        assert_eq!(
            toggle_select_all(&["a1".to_owned()], &sessions),
            ["a1", "b1"]
        );
        // 名字对不上的一律不算勾上：陈旧的 id 不能把「全选」的状态凑出来。
        assert!(all_selected(
            &["a1".to_owned(), "gone".to_owned(), "b1".to_owned()],
            &sessions
        ));
        assert!(!all_selected(&["a1".to_owned()], &sessions));
    }
}
