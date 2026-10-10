//! 变更视图：左右双栏逐行对齐，改动行带底色与语法高亮。
//!
//! 对齐在加载时算一次（[`aligned_diff`]），渲染只读结果；任一侧被截断时不给双栏，回落到
//! [`render_diff`] 的统一 diff 文本——两侧各自截断到不同位置时，逐行对齐只会在截断边界
//! 附近凭空多出一行替换，比没有颜色更误导。
//!
//! 两栏各自是一份可选文档：左栏的行在窗口选择的 `document_order` 上排在右栏之前
//! （见 [`document_order`]），在一栏里纵向拖动因此只选中这一栏的行，不会把对面同序的那一行
//! 一并带上。
//!
//! 只画看得见的那一段行（与正文栏同一套做法，见 [`super::editor::visible_rows`]）：双栏一行
//! 两个选择参与者，而窗口级选择每画一行都要问一次全窗的选中文本，整份铺出来就是行数的平方。
//!
//! 行分类复用 [`crate::tool_view::DiffLineKind`]、配色复用 [`crate::views::chat::diff_colors`]，
//! 行内高亮与行号复用 [`super::editor`] 的原语，与正文栏是同一套观感。

use std::ops::Range;

use astrcode_protocol::http::{FileChangeStateDto, FileDiffResponseDto};
use gpui_kit::{
    AnyElement, App, HighlightStyle, IntoElement, ParentElement as _, Pixels, SharedString,
    Styled as _, TextStyle, Window,
    component::{ActiveTheme as _, h_flex, highlighter::HighlightTheme, v_flex},
    div, px,
};
use similar::{DiffOp, TextDiff};

use super::{
    editor::{GUTTER_WIDTH, VERTICAL_PADDING, clip_to_line, highlight, vertical_space},
    selectable::SelectableLine,
};
use crate::{
    tool_view::{DiffLineKind, diff_line_kind},
    views::chat::diff_colors,
};

/// 统一 diff 回退路径一次渲染的行数上限；超出只给一句提示。
const MAX_RENDERED_DIFF_LINES: usize = 2000;

/// 双栏一次画的行数上限；超出的部分只报数。
const MAX_ALIGNED_ROWS: usize = 2000;

/// 每个改动块两侧保留的上下文行数。
const CONTEXT_RADIUS: usize = 3;

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

/// 一次对齐的全部结果。
pub(super) struct AlignedDiff {
    /// 要画的行，按上下顺序；折叠占位也在其中。
    pub(super) rows: Vec<AlignedRow>,
    /// 因行数上限没画出来的行数；0 表示全都画了。
    pub(super) omitted: usize,
}

/// 双栏里的一行。
pub(super) enum AlignedRow {
    /// 内容行：左右各一格，没有对应行的一侧是空白格。
    Cells { left: DiffCell, right: DiffCell },
    /// 折叠占位：两个改动块之间被跳过的相同行数。
    Fold(usize),
}

/// 双栏里的一格。
pub(super) struct DiffCell {
    /// 1 起的行号；`None` 表示这一侧没有对应行。
    pub(super) number: Option<usize>,
    /// 行文本，行尾换行符已去掉。
    pub(super) text: SharedString,
    /// 行内高亮，下标相对 [`Self::text`]。
    pub(super) styles: Vec<(Range<usize>, HighlightStyle)>,
    /// 这一格的语义色；`None` 表示这一侧没有对应行，只留空白。
    pub(super) kind: Option<DiffLineKind>,
}

impl DiffCell {
    /// 没有对应行的一格。
    fn blank() -> Self {
        Self {
            number: None,
            text: SharedString::default(),
            styles: Vec::new(),
            kind: None,
        }
    }

    /// 按行号取一格：`index` 为 `None`（或越界）时给空白格。
    fn new(
        index: Option<usize>,
        text: &str,
        ranges: &[Range<usize>],
        styles: &[(Range<usize>, HighlightStyle)],
        kind: DiffLineKind,
    ) -> Self {
        let Some((number, range)) =
            index.and_then(|index| ranges.get(index).cloned().map(|range| (index + 1, range)))
        else {
            return Self::blank();
        };
        // `similar` 的行切片把行尾换行符含在里面，样式也只能裁到去掉换行符之后的那一段：
        // 超出了这一行文本的样式区间对 `StyledText` 没有意义。
        let line =
            range.start..range.start + text[range.clone()].trim_end_matches(['\n', '\r']).len();
        Self {
            number: Some(number),
            text: SharedString::from(text[line.clone()].to_owned()),
            styles: clip_to_line(&line, styles),
            kind: Some(kind),
        }
    }
}

/// 把 HEAD 侧与工作区侧两份全文对齐成双栏要画的行。
///
/// 只保留改动块与它两侧各 [`CONTEXT_RADIUS`] 行上下文，块之间折成 [`AlignedRow::Fold`]：
/// 整份逐行画没有意义，用户要看的只是改了什么。行数超过 [`MAX_ALIGNED_ROWS`] 的部分不再
/// 画，由 [`AlignedDiff::omitted`] 报数。
pub(super) fn aligned_diff(
    original: &str,
    modified: &str,
    language: &str,
    theme: &HighlightTheme,
) -> AlignedDiff {
    let diff = TextDiff::from_lines(original, modified);
    // 两侧各只高亮一次，再按行裁剪；逐行各跑一遍语法分析会慢得多（同 `editor::highlight`）。
    let old_styles = highlight(original, language, theme);
    let new_styles = highlight(modified, language, theme);
    let old_ranges = line_ranges(diff.iter_old_slices());
    let new_ranges = line_ranges(diff.iter_new_slices());

    let mut rows: Vec<AlignedRow> = Vec::new();
    let mut omitted = 0usize;
    let mut push = |row: AlignedRow| {
        if rows.len() < MAX_ALIGNED_ROWS {
            rows.push(row);
        } else {
            omitted += 1;
        }
    };

    // 上一段之后、旧侧已经画到哪一行：两个改动块之间被跳过的行数由它算出来。
    let mut drawn_old_end = 0usize;
    for (index, group) in diff.grouped_ops(CONTEXT_RADIUS).iter().enumerate() {
        let start = group.first().map_or(0, |op| op.old_range().start);
        if index > 0 && start > drawn_old_end {
            push(AlignedRow::Fold(start - drawn_old_end));
        }
        for op in group {
            match *op {
                DiffOp::Equal {
                    old_index,
                    new_index,
                    len,
                } => {
                    for offset in 0..len {
                        push(AlignedRow::Cells {
                            left: DiffCell::new(
                                Some(old_index + offset),
                                original,
                                &old_ranges,
                                &old_styles,
                                DiffLineKind::Context,
                            ),
                            right: DiffCell::new(
                                Some(new_index + offset),
                                modified,
                                &new_ranges,
                                &new_styles,
                                DiffLineKind::Context,
                            ),
                        });
                    }
                },
                DiffOp::Delete {
                    old_index, old_len, ..
                } => {
                    for offset in 0..old_len {
                        push(AlignedRow::Cells {
                            left: DiffCell::new(
                                Some(old_index + offset),
                                original,
                                &old_ranges,
                                &old_styles,
                                DiffLineKind::Deletion,
                            ),
                            right: DiffCell::blank(),
                        });
                    }
                },
                DiffOp::Insert {
                    new_index, new_len, ..
                } => {
                    for offset in 0..new_len {
                        push(AlignedRow::Cells {
                            left: DiffCell::blank(),
                            right: DiffCell::new(
                                Some(new_index + offset),
                                modified,
                                &new_ranges,
                                &new_styles,
                                DiffLineKind::Addition,
                            ),
                        });
                    }
                },
                // 替换逐行配对：两侧都存在时一行对一行地摆开，多出来的一侧接在下面。
                DiffOp::Replace {
                    old_index,
                    old_len,
                    new_index,
                    new_len,
                } => {
                    let paired = old_len.min(new_len);
                    for offset in 0..paired {
                        push(AlignedRow::Cells {
                            left: DiffCell::new(
                                Some(old_index + offset),
                                original,
                                &old_ranges,
                                &old_styles,
                                DiffLineKind::Deletion,
                            ),
                            right: DiffCell::new(
                                Some(new_index + offset),
                                modified,
                                &new_ranges,
                                &new_styles,
                                DiffLineKind::Addition,
                            ),
                        });
                    }
                    for offset in paired..old_len {
                        push(AlignedRow::Cells {
                            left: DiffCell::new(
                                Some(old_index + offset),
                                original,
                                &old_ranges,
                                &old_styles,
                                DiffLineKind::Deletion,
                            ),
                            right: DiffCell::blank(),
                        });
                    }
                    for offset in paired..new_len {
                        push(AlignedRow::Cells {
                            left: DiffCell::blank(),
                            right: DiffCell::new(
                                Some(new_index + offset),
                                modified,
                                &new_ranges,
                                &new_styles,
                                DiffLineKind::Addition,
                            ),
                        });
                    }
                },
            }
        }
        if let Some(op) = group.last() {
            drawn_old_end = op.old_range().end;
        }
    }

    AlignedDiff { rows, omitted }
}

/// 按 `similar` 自己的行切分算出每行的字节区间，下标与 diff 里的行号一一对应。
///
/// 不能拿 [`super::editor::line_ranges`] 代替：那个按 `\n` 切（与 `str::lines` 同口径），而
/// `similar` 连单独的 `\r` 也当行尾，只有老式 Mac 换行的文件上两者行数会不同，用错会整体错位。
fn line_ranges<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0usize;
    for line in lines {
        let end = start + line.len();
        ranges.push(start..end);
        start = end;
    }
    ranges
}

/// 渲染统一 diff 正文（回退路径）。
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

/// 渲染双栏里 `rows` 这一段。
///
/// 底色承担增删的信号（语义色已与主题底色预混成不透明色，见
/// [`crate::views::chat::diff_colors`]），行内文本仍是语法色——两种信息不互相盖掉。
///
/// 只画 `rows` 这一段，其余行由上下两段留白占住高度（与 [`super::editor::render_code`] 同一
/// 做法）：每一行都正好一个行高，留白与滚动位置才对得上。`rows` 由调用点按滚动位置算。
pub(super) fn render_aligned(
    aligned: &AlignedDiff,
    rows: Range<usize>,
    window: &Window,
    cx: &App,
) -> AnyElement {
    // 两栏共用一份默认样式：高亮没盖到的空隙（空白、缩进）用它上色。
    let mut default_style = window.text_style();
    default_style.font_family = cx.theme().mono_font_family.clone();
    default_style.color = cx.theme().foreground;

    let line_height = window.line_height();
    let total = aligned.rows.len();
    // 调用方按同一个总行数算的区间，这里再夹一次：越界下标取行会直接 panic。
    let rows = rows.start.min(total)..rows.end.min(total);

    let digits = gutter_digits(aligned);
    let mut column = v_flex().w_full().min_w_0().child(vertical_space(
        px(VERTICAL_PADDING) + line_height * rows.start as f32,
    ));
    for (offset, row) in aligned.rows[rows.clone()].iter().enumerate() {
        // 绝对行号：元素 id 与选择次序都取它，两样都必须跨帧稳定。
        let index = rows.start + offset;
        column = column.child(match row {
            AlignedRow::Cells { left, right } => {
                render_cells(index, left, right, digits, total, &default_style, cx)
            },
            AlignedRow::Fold(count) => render_fold(*count, line_height, cx),
        });
    }
    column = column.child(vertical_space(
        line_height * (total - rows.end) as f32 + px(VERTICAL_PADDING),
    ));
    if aligned.omitted > 0 {
        column = column.child(
            div()
                .px_3()
                .py_2()
                .text_color(cx.theme().muted_foreground)
                .child(format!(
                    "（只渲染前 {MAX_ALIGNED_ROWS} 行，另有 {} 行未显示）",
                    aligned.omitted
                )),
        );
    }
    column.into_any_element()
}

/// 行号列按整份对齐结果里的最大行号补空格：等宽字体下这等价于右对齐，且不依赖文本对齐 API
/// （与 [`super::editor::render_code`] 同一做法）。
///
/// 取整份而不是当前这一段：只画一段时若按可见行数来补空格，滚动中行号列会忽宽忽窄。
fn gutter_digits(aligned: &AlignedDiff) -> usize {
    let widest = aligned
        .rows
        .iter()
        .flat_map(|row| match row {
            AlignedRow::Cells { left, right } => [left.number, right.number],
            AlignedRow::Fold(_) => [None, None],
        })
        .flatten()
        .max()
        .unwrap_or(1);
    widest.to_string().len()
}

/// 双栏里的一栏。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffSide {
    Left,
    Right,
}

/// 左右两栏在窗口选择里的次序：左栏占前一段、右栏占后一段。
///
/// 选择层按 `document_order` 判断「哪些参与者落在拖动的两端之间」：两栏交错编号（一行两个，
/// 左奇右偶）时，在一栏里纵向拖动会把对面同序的那几行一并算进选择，两侧一起变蓝。各占一段
/// 之后，同一栏内的拖动只覆盖这一栏，两栏于是各是一份可选文档。
fn document_order(side: DiffSide, row: usize, total: usize) -> u64 {
    let row = row as u64;
    match side {
        DiffSide::Left => row,
        DiffSide::Right => total as u64 + row,
    }
}

/// 一行内容：左格、1px 分隔线、右格。
///
/// 分隔线自绘而不是用边框：只有 1px，用边框会把相邻两栏画成两条线（与
/// [`super::CodeView`] 里两栏之间的那道线同一条做法）。
fn render_cells(
    row: usize,
    left: &DiffCell,
    right: &DiffCell,
    digits: usize,
    total: usize,
    default_style: &TextStyle,
    cx: &App,
) -> AnyElement {
    h_flex()
        .w_full()
        .min_w_0()
        .flex_shrink_0()
        .child(render_cell(
            format!("diff-left-{row}"),
            document_order(DiffSide::Left, row, total),
            left,
            digits,
            default_style,
            cx,
        ))
        .child(
            div()
                .w(px(1.0))
                .flex_shrink_0()
                .self_stretch()
                .bg(cx.theme().border),
        )
        .child(render_cell(
            format!("diff-right-{row}"),
            document_order(DiffSide::Right, row, total),
            right,
            digits,
            default_style,
            cx,
        ))
        .into_any_element()
}

/// 一格：行号槽 + 该行文本。这一侧没有对应行时只有底色，没有文本。
fn render_cell(
    id: String,
    order: u64,
    cell: &DiffCell,
    digits: usize,
    default_style: &TextStyle,
    cx: &App,
) -> AnyElement {
    // 空白格不上色：它表达的是「这一侧没有这一行」，着色会把它说成一次改动。
    let background = cell
        .kind
        .map_or(cx.theme().transparent, |kind| diff_colors(kind, cx).1);
    let number = cell
        .number
        .map_or_else(String::new, |number| format!("{number:>digits$}"));

    let mut element = h_flex()
        .flex_1()
        .min_w_0()
        .whitespace_nowrap()
        .bg(background)
        .child(
            div()
                .flex_shrink_0()
                .w(px(GUTTER_WIDTH))
                .pr_2()
                .whitespace_nowrap()
                .text_color(cx.theme().muted_foreground)
                .child(number),
        );
    if cell.text.is_empty() {
        return element.into_any_element();
    }
    // 行内文本不换行：换行会让左右两栏的行对不上（与 [`super::editor::render_code`] 同一条做法）。
    element = element.child(
        div()
            .min_w_0()
            .whitespace_nowrap()
            .child(SelectableLine::new(
                SharedString::from(id),
                order,
                cell.text.clone(),
                cell.styles.clone(),
                default_style.clone(),
            )),
    );
    element.into_any_element()
}

/// 折叠占位：横跨两栏的一条说明。
///
/// 高度钉死一个行高：上下留白按「每行一个行高」算，折叠占位只要与行高差一点，滚动位置就会
/// 与实际行对不上（见 [`render_aligned`]）。
fn render_fold(count: usize, line_height: Pixels, cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .h(line_height)
        .items_center()
        .px_3()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(format!("⋯ 折叠 {count} 行相同内容"))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui_kit::{
        AppContext as _, Context, FocusHandle, Modifiers, MouseButton, Render, ScrollHandle,
        TestAppContext,
        base::{TextSelection, TextSelectionLayer},
        point,
    };

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
            original: String::new(),
            original_truncated: false,
            insertions: 12,
            deletions: 3,
            truncated: false,
        };
        assert_eq!(change_summary(&diff), "+12 −3");
        diff.insertions = 0;
        diff.deletions = 0;
        assert_eq!(change_summary(&diff), "+0 −0");
    }

    /// 对齐一份 rust 正文；语言取 `rust` 是为了同时验证行内高亮。
    fn aligned(original: &str, modified: &str) -> AlignedDiff {
        let theme = HighlightTheme::default_dark();
        aligned_diff(original, modified, "rust", theme.as_ref())
    }

    /// 第 `index` 行的两格；不是内容行即失败。
    fn cells(aligned: &AlignedDiff, index: usize) -> (&DiffCell, &DiffCell) {
        match &aligned.rows[index] {
            AlignedRow::Cells { left, right } => (left, right),
            AlignedRow::Fold(count) => panic!("第 {index} 行是折叠占位（{count} 行），不是内容行"),
        }
    }

    /// 第 `index` 行折叠了多少行；不是折叠占位即失败。
    fn fold(aligned: &AlignedDiff, index: usize) -> usize {
        match &aligned.rows[index] {
            AlignedRow::Fold(count) => *count,
            AlignedRow::Cells { .. } => panic!("第 {index} 行是内容行，不是折叠占位"),
        }
    }

    /// 纯新增：左侧全空、右侧逐行是新增，行号从 1 起。
    #[test]
    fn a_pure_addition_leaves_the_left_column_empty() {
        let aligned = aligned("", "one\ntwo\n");

        assert_eq!(aligned.rows.len(), 2);
        assert_eq!(aligned.omitted, 0);
        for (index, text) in ["one", "two"].iter().enumerate() {
            let (left, right) = cells(&aligned, index);
            assert_eq!(left.kind, None);
            assert_eq!(left.number, None);
            assert_eq!(left.text.as_ref(), "");
            assert_eq!(right.kind, Some(DiffLineKind::Addition));
            assert_eq!(right.number, Some(index + 1));
            assert_eq!(right.text.as_ref(), *text);
        }
    }

    /// 纯删除：右侧全空、左侧逐行是删除。
    #[test]
    fn a_pure_deletion_leaves_the_right_column_empty() {
        let aligned = aligned("one\ntwo\n", "");

        assert_eq!(aligned.rows.len(), 2);
        for (index, text) in ["one", "two"].iter().enumerate() {
            let (left, right) = cells(&aligned, index);
            assert_eq!(left.kind, Some(DiffLineKind::Deletion));
            assert_eq!(left.number, Some(index + 1));
            assert_eq!(left.text.as_ref(), *text);
            assert_eq!(right.kind, None);
            assert_eq!(right.number, None);
        }
    }

    /// 改一行：两侧同一行对齐，左侧是删除、右侧是新增。
    #[test]
    fn a_one_line_change_pairs_both_sides_on_one_row() {
        let aligned = aligned("one\ntwo\nthree\n", "one\n2\nthree\n");

        assert_eq!(aligned.rows.len(), 3);
        let (left, right) = cells(&aligned, 0);
        assert_eq!(left.kind, Some(DiffLineKind::Context));
        assert_eq!(right.kind, Some(DiffLineKind::Context));
        let (left, right) = cells(&aligned, 1);
        assert_eq!(left.kind, Some(DiffLineKind::Deletion));
        assert_eq!(left.number, Some(2));
        assert_eq!(left.text.as_ref(), "two");
        assert_eq!(right.kind, Some(DiffLineKind::Addition));
        assert_eq!(right.number, Some(2));
        assert_eq!(right.text.as_ref(), "2");
        let (left, right) = cells(&aligned, 2);
        assert_eq!(left.kind, Some(DiffLineKind::Context));
        assert_eq!(left.number, Some(3));
        assert_eq!(right.number, Some(3));
    }

    /// 改行比原来多：配不上的那一侧补空白格，而不是错位。
    #[test]
    fn a_change_that_adds_lines_pads_the_shorter_side() {
        let aligned = aligned("one\ntwo\n", "one\n2\n2.5\n");

        assert_eq!(aligned.rows.len(), 3);
        let (left, right) = cells(&aligned, 1);
        assert_eq!(left.kind, Some(DiffLineKind::Deletion));
        assert_eq!(right.kind, Some(DiffLineKind::Addition));
        let (left, right) = cells(&aligned, 2);
        assert_eq!(left.kind, None);
        assert_eq!(left.number, None);
        assert_eq!(right.kind, Some(DiffLineKind::Addition));
        assert_eq!(right.text.as_ref(), "2.5");
    }

    /// 相距很远的两次改动：中间折成一条占位，行数还要对得上。
    ///
    /// `l1` 与 `l18` 之间的 16 行相同内容，两侧各留 [`CONTEXT_RADIUS`] 行上下文，因此折起
    /// 来的正好是 16 − 3 − 3 行。
    #[test]
    fn a_distant_change_folds_the_lines_in_between() {
        let original: String = (0..20).map(|index| format!("l{index}\n")).collect();
        let mut modified: Vec<String> = (0..20).map(|index| format!("l{index}\n")).collect();
        modified[1] = "x1\n".to_owned();
        modified[18] = "x18\n".to_owned();
        let modified = modified.concat();

        let aligned = aligned(&original, &modified);

        assert_eq!(aligned.rows.len(), 11);
        assert_eq!(fold(&aligned, 5), 10);
        // 折叠两侧各留三行上下文。
        for offset in 2..5 {
            let (left, _) = cells(&aligned, offset);
            assert_eq!(left.kind, Some(DiffLineKind::Context));
            assert_eq!(left.text.as_ref(), format!("l{offset}"));
        }
        for offset in 0..3 {
            let (left, _) = cells(&aligned, 6 + offset);
            assert_eq!(left.kind, Some(DiffLineKind::Context));
            assert_eq!(left.number, Some(16 + offset));
        }
    }

    /// 行内高亮要落在这一行的文本范围内：越界的样式区间对 `StyledText` 没有意义。
    #[test]
    fn cell_styles_stay_inside_the_line_text() {
        let aligned = aligned("fn main() {}\n", "fn main() { let x = 1; }\n");

        let (_, right) = cells(&aligned, 0);
        assert!(!right.styles.is_empty(), "rust 行应当有语法高亮");
        for (range, _) in &right.styles {
            assert!(
                range.end <= right.text.len(),
                "高亮区间 {:?} 超出了这一行的 {} 字节",
                range,
                right.text.len()
            );
        }
    }

    /// 行数超上限时只画前 [`MAX_ALIGNED_ROWS`] 行，剩下的只报数。
    #[test]
    fn the_row_cap_reports_what_is_left_out() {
        let extra = 50;
        let original: String = (0..MAX_ALIGNED_ROWS + extra)
            .map(|index| format!("old {index}\n"))
            .collect();
        let modified: String = (0..MAX_ALIGNED_ROWS + extra)
            .map(|index| format!("new {index}\n"))
            .collect();

        let aligned = aligned(&original, &modified);

        assert_eq!(aligned.rows.len(), MAX_ALIGNED_ROWS);
        // 整份都改了：配完前 2000 行，每行还剩一行没画。
        assert_eq!(aligned.omitted, extra);
    }

    /// 双栏要能真的画出来：拼装时的错误（越界的高亮区间、对不上的行号）在这里就会炸。
    ///
    /// 编译通过只证明元素类型对得上，证明不了 `SelectableLine` 收得下这些区间。
    #[gpui_kit::test]
    fn the_two_columns_actually_build(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        let (_, cx) = cx.add_window_view(|_, _| TwoColumns);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// 只画看得见的那一段时，行号与选择次序仍取绝对行号：跨帧滚动时同一条元素 id 不能被别的
    /// 行顶掉，否则选择状态会跟着换行。
    #[gpui_kit::test]
    fn a_scrolled_band_keeps_absolute_row_orders(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        let (_, cx) = cx.add_window_view(|_, _| ScrolledBand);

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// 在一栏里纵向拖动只选中这一栏：对面同序的那一行不该跟着被选中。
    ///
    /// 这是 [`document_order`] 那条不变式的端到端检查——拖动走的是窗口选择层，而选择层只认
    /// 次序，不认「栏」，所以「只选一栏」这句话得由真的选一遍来证明。
    #[gpui_kit::test]
    fn a_drag_inside_one_column_stays_in_that_column(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        let (_, cx) = cx.add_window_view(|_, _| TwoColumns);
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // 左栏第一行拖到第二行：x 落在行号槽右侧的正文上，y 落在行中。
        let line_height = cx.update(|window, _| f32::from(window.line_height()));
        let first = point(px(60.), px(VERTICAL_PADDING + line_height * 0.5));
        let second = point(px(60.), px(VERTICAL_PADDING + line_height * 1.5));
        cx.simulate_mouse_down(first, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(second, Some(MouseButton::Left), Modifiers::default());
        cx.simulate_mouse_up(second, MouseButton::Left, Modifiers::default());

        cx.update(|window, cx| {
            let _ = window.draw(cx);
            let selected = TextSelection::selected_text(window, cx);
            assert!(
                selected.contains("main() {}"),
                "左栏第一行的正文应当在选中结果里，实际选中 {selected:?}"
            );
            assert!(
                !selected.contains("y = 2") && !selected.contains("extra"),
                "右栏的正文不该被带上，实际选中 {selected:?}"
            );
        });
    }

    /// 两栏的次序各占一段：同一栏内的拖动不会带出对面的行。
    ///
    /// 选择层按 `document_order` 判断「谁落在拖动的两端之间」，所以这条不变式就是「只选中一栏」
    /// 的全部含义。
    #[test]
    fn the_two_columns_own_disjoint_order_ranges() {
        let total = 5;
        let left: Vec<u64> = (0..total)
            .map(|row| document_order(DiffSide::Left, row, total))
            .collect();
        let right: Vec<u64> = (0..total)
            .map(|row| document_order(DiffSide::Right, row, total))
            .collect();

        assert_eq!(left, [0, 1, 2, 3, 4]);
        assert_eq!(right, [5, 6, 7, 8, 9]);
        // 在左栏里从第 2 行拖到第 4 行：落在两端之间的次序全部属于左栏。
        let dragged = left[1]..=left[3];
        assert!(
            !right.iter().any(|order| dragged.contains(order)),
            "左栏内拖动不该带上右栏"
        );
    }

    /// 只画一段时滚动量程仍与整份一致：留白替没画的行占住高度。
    ///
    /// 与正文栏的同一条不变式（见 `editor` 的 `spacers_keep_the_scroll_range_of_the_whole_body`）
    /// 一样：量程与「这一帧画了哪一段」无关，否则滚到底会露白、滚到一半会跳。
    #[gpui_kit::test]
    fn spacers_keep_the_scroll_range_of_the_whole_diff(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });

        // 内容高度 = 滚动量程 + 视口高度，两者都由框架在布局时量好。
        let probe = |cx: &mut TestAppContext, rows: Range<usize>| -> (f32, f32) {
            let scroll = ScrollHandle::new();
            let (_, cx) = cx.add_window_view(|window, cx| {
                let probe = cx.new(|cx| BandProbe {
                    aligned: long_diff(),
                    rows: rows.clone(),
                    focus: cx.focus_handle(),
                    scroll: scroll.clone(),
                });
                gpui_kit::base::Root::new(probe, window, cx)
            });
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
            let line_height = cx.update(|window, _| f32::from(window.line_height()));
            (
                f32::from(scroll.max_offset().y) + f32::from(scroll.bounds().size.height),
                line_height,
            )
        };

        let aligned = long_diff();
        assert!(
            aligned
                .rows
                .iter()
                .any(|row| matches!(row, AlignedRow::Fold(_))),
            "夹具要带一个折叠占位，否则折叠行的高度没被这条测试盖到"
        );
        let total = aligned.rows.len();
        let (full, line_height) = probe(cx, 0..total);
        let (windowed, _) = probe(cx, 0..8);
        let expected = line_height * total as f32 + 2.0 * VERTICAL_PADDING;
        assert!(
            (full - expected).abs() <= 1.0,
            "整份渲染的内容高度 {full} 与「行高 × 行数 + 上下内边距」{expected} 不一致"
        );
        assert!(
            (full - windowed).abs() <= 1.0,
            "只渲染 8 行时的滚动量程应仍与整份一致：全量 {full}，窗口 {windowed}"
        );
    }

    /// 一份「改一行 + 补两行」的双栏，供上面那条冒烟测试画一遍。
    struct TwoColumns;

    impl Render for TwoColumns {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let aligned = aligned(
                "fn main() {}\nlet x = 1;\n",
                "fn main() { let y = 2; }\nlet x = 1;\n\nfn extra() {}\n",
            );
            let rows = 0..aligned.rows.len();
            div()
                .size_full()
                .child(TextSelectionLayer)
                .child(render_aligned(&aligned, rows, window, cx))
        }
    }

    /// 同一份双栏，只画中间那一段：留白与元素拼装都要经得起非零起点。
    struct ScrolledBand;

    impl Render for ScrolledBand {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let aligned = aligned(
                "fn main() {}\nlet x = 1;\nlet y = 2;\n",
                "fn main() { let y = 2; }\nlet x = 1;\n\nfn extra() {}\n",
            );
            div()
                .size_full()
                .child(TextSelectionLayer)
                .child(render_aligned(&aligned, 1..3, window, cx))
        }
    }

    /// 量滚动量程用的最小双栏：一个能拿焦点的滚动容器加一份被虚拟化的双栏。
    struct BandProbe {
        aligned: AlignedDiff,
        rows: Range<usize>,
        focus: FocusHandle,
        scroll: ScrollHandle,
    }

    impl Render for BandProbe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(TextSelectionLayer)
                .child(super::super::editor::pane_scroll(
                    &self.focus,
                    &self.scroll,
                    render_aligned(&self.aligned, self.rows.clone(), window, cx),
                ))
        }
    }

    /// 300 行、两处改动夹着 100 行相同内容：既量得出量程，也带一个折叠占位。
    ///
    /// 折叠占位必须与内容行一样是一个行高，留白公式才成立——把它放进夹具，这条测试就顺带钉住了
    /// 那件事。
    fn long_diff() -> AlignedDiff {
        let original: String = (0..300).map(|index| format!("old {index}\n")).collect();
        let modified: String = (0..300)
            .map(|index| {
                if (100..200).contains(&index) {
                    format!("old {index}\n")
                } else {
                    format!("new {index}\n")
                }
            })
            .collect();
        aligned(&original, &modified)
    }
}
