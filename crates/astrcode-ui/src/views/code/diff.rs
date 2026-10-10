//! 变更视图：把服务端给的统一 diff 按行着色渲染。
//!
//! 行分类复用 [`crate::tool_view::diff_line_kind`]、配色复用 [`crate::views::chat::diff_colors`]，
//! 与聊天里工具卡的 diff 预览是同一套观感；这里只多了滚动容器与状态说明。

use astrcode_protocol::http::{FileChangeStateDto, FileDiffResponseDto};
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement as _, Styled as _,
    component::{ActiveTheme as _, v_flex},
    div,
};

use crate::{tool_view::diff_line_kind, views::chat::diff_colors};

/// 一次渲染的 diff 行数上限；超出只给一句提示。
const MAX_RENDERED_DIFF_LINES: usize = 2000;

/// 状态标签：页头用它标出当前文件相对 HEAD 的处境。
pub(super) fn state_label(state: FileChangeStateDto) -> &'static str {
    match state {
        FileChangeStateDto::Modified => "已修改",
        FileChangeStateDto::Untracked => "新文件",
        FileChangeStateDto::Unchanged => "无改动",
        FileChangeStateDto::Binary => "二进制",
        FileChangeStateDto::NotARepository => "非 git 仓库",
        FileChangeStateDto::GitUnavailable => "拿不到 git",
    }
}

/// 没有 diff 正文可画时的说明；有正文时返回 `None`。
pub(super) fn state_note(state: FileChangeStateDto) -> Option<&'static str> {
    match state {
        FileChangeStateDto::Modified | FileChangeStateDto::Untracked => None,
        FileChangeStateDto::Unchanged => Some("与 HEAD 一致，没有未提交的改动。"),
        FileChangeStateDto::Binary => Some("二进制文件，无法按行比较。"),
        FileChangeStateDto::NotARepository => Some("这个目录不在 git 工作树里，没有可比对的基线。"),
        FileChangeStateDto::GitUnavailable => Some("拿不到 git 的改动，请确认 git 可用。"),
    }
}

/// 增删行数摘要，例如 `+12 −3`。
pub(super) fn change_summary(diff: &FileDiffResponseDto) -> String {
    format!("+{} −{}", diff.insertions, diff.deletions)
}

/// 渲染变更正文。
///
/// 每行一支：底色是语义色调淡，字色是该行的语义色。两者都画在行 `div` 上（`text_color` +
/// `bg`），由行内的文本继承——与 [`crate::views::chat`] 里工具卡的 diff 预览同一条路径，
/// 那里的颜色是可见的，`StyledText` 的默认样式那条路走不出颜色。
pub(super) fn render_diff(diff: &FileDiffResponseDto, cx: &App) -> AnyElement {
    if let Some(note) = state_note(diff.state) {
        return div()
            .p_4()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(note)
            .into_any_element();
    }

    let lines: Vec<&str> = diff.unified_diff.lines().collect();
    let rendered = lines.len().min(MAX_RENDERED_DIFF_LINES);
    let mut column = v_flex().w_full().min_w_0().py_2();
    for line in lines.iter().take(rendered) {
        let (color, background) = diff_colors(diff_line_kind(line), cx);
        column = column.child(
            div()
                .w_full()
                .min_w_0()
                .whitespace_nowrap()
                .font_family(cx.theme().mono_font_family.clone())
                .text_color(color)
                .bg(background)
                .child(line.to_string()),
        );
    }

    if lines.len() > rendered {
        column = column.child(
            div()
                .px_3()
                .py_2()
                .text_color(cx.theme().muted_foreground)
                .child(format!("（只渲染前 {rendered} 行）")),
        );
    }
    column.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 有正文的状态不能带说明，否则界面上会同时出现 diff 和「没有改动」。
    #[test]
    fn only_states_without_a_diff_carry_a_note() {
        assert_eq!(state_note(FileChangeStateDto::Modified), None);
        assert_eq!(state_note(FileChangeStateDto::Untracked), None);
        for state in [
            FileChangeStateDto::Unchanged,
            FileChangeStateDto::Binary,
            FileChangeStateDto::NotARepository,
            FileChangeStateDto::GitUnavailable,
        ] {
            assert!(state_note(state).is_some(), "{state:?} 应该有说明");
        }
    }

    #[test]
    fn summary_counts_both_sides() {
        let mut diff = FileDiffResponseDto {
            path: "src/lib.rs".to_owned(),
            state: FileChangeStateDto::Modified,
            unified_diff: String::new(),
            insertions: 12,
            deletions: 3,
            truncated: false,
        };
        assert_eq!(change_summary(&diff), "+12 −3");
        diff.insertions = 0;
        diff.deletions = 0;
        assert_eq!(change_summary(&diff), "+0 −0");
    }
}
