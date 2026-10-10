//! 变更清单：整个工作区相对 git HEAD 的未提交改动。
//!
//! 这里只有推导，渲染在 [`super`]；条目顺序由服务端按 git 自己的输出给，没有可再排的东西，
//! 因此不像 [`super::tree`] 那样需要算「可见行」。

use astrcode_protocol::http::{
    GitStatusAvailabilityDto, GitStatusEntryStateDto, GitStatusResponseDto,
};

/// 条目的状态标签。
pub(super) fn entry_label(state: GitStatusEntryStateDto) -> &'static str {
    match state {
        GitStatusEntryStateDto::Modified => "修改",
        GitStatusEntryStateDto::Added => "新增",
        GitStatusEntryStateDto::Deleted => "删除",
        GitStatusEntryStateDto::Renamed => "重命名",
        GitStatusEntryStateDto::Untracked => "未跟踪",
        GitStatusEntryStateDto::Conflicted => "冲突",
    }
}

/// 清单列不出来时的说明；清单可用时返回 `None`（空清单由调用点另给一句）。
pub(super) fn availability_note(availability: GitStatusAvailabilityDto) -> Option<&'static str> {
    match availability {
        GitStatusAvailabilityDto::Available => None,
        GitStatusAvailabilityDto::NotARepository => Some("这个目录不在 git 工作树里。"),
        GitStatusAvailabilityDto::GitUnavailable => Some("拿不到 git 的状态，请确认 git 可用。"),
    }
}

/// 区标题：数得出条目时带上条数，被截断时补一个 `+`。
pub(super) fn section_label(status: Option<&GitStatusResponseDto>) -> String {
    match status {
        Some(status) if !status.entries.is_empty() => {
            let overflow = if status.truncated { "+" } else { "" };
            format!("变更 {}{overflow}", status.entries.len())
        },
        _ => "变更".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::http::GitStatusEntryDto;

    use super::*;

    /// 清单取不到时必须有说明：没有说明的空清单会被读成「没有改动」。
    #[test]
    fn unavailable_status_explains_itself() {
        assert_eq!(availability_note(GitStatusAvailabilityDto::Available), None);
        for availability in [
            GitStatusAvailabilityDto::NotARepository,
            GitStatusAvailabilityDto::GitUnavailable,
        ] {
            assert!(
                availability_note(availability).is_some(),
                "{availability:?} 应该有说明"
            );
        }
    }

    /// 数得出条目才写条数；那个 `+` 是「还有条目没列出来」的唯一提示。
    #[test]
    fn section_label_counts_entries_and_marks_truncation() {
        assert_eq!(section_label(None), "变更");
        assert_eq!(section_label(Some(&status(0, false))), "变更");
        assert_eq!(section_label(Some(&status(3, false))), "变更 3");
        assert_eq!(section_label(Some(&status(20, true))), "变更 20+");
    }

    fn status(count: usize, truncated: bool) -> GitStatusResponseDto {
        GitStatusResponseDto {
            availability: GitStatusAvailabilityDto::Available,
            entries: (0..count)
                .map(|index| GitStatusEntryDto {
                    path: format!("file{index}.rs"),
                    state: GitStatusEntryStateDto::Modified,
                })
                .collect(),
            truncated,
        }
    }
}
