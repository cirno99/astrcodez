//! 只读代码正文：整份正文高亮一次，按行切成行内片段后逐行渲染。
//!
//! 这里没有可编辑的输入面，也没有光标：正文只用来「看」，因此不存在误改的可能。
//! 高亮来自 gpui-component 的 tree-sitter 高亮器，语言按文件扩展名推断。
//!
//! 行号与代码之间那条窄条是相对 HEAD 的变更标记（绿=新增、黄=改动、红=删除），行本身
//! 参与窗口级文本选择；两者的来源见 [`super::annotate`] 与 [`super::selectable`]。
//!
//! 行切分与样式裁剪都是纯函数（不碰窗口），可以脱离 GPU 测试；只有最后的元素拼装需要 `Window`。

use std::ops::Range;

use gpui_kit::{
    AnyElement, App, HighlightStyle, Hsla, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window,
    component::{
        ActiveTheme as _, h_flex,
        highlighter::{HighlightTheme, SyntaxHighlighter},
        v_flex,
    },
    div, px,
};
use ropey::Rope;

use super::{annotate::LineChange, selectable::SelectableLine};

/// 一次渲染的行数上限。
///
/// 正文可能有几十万行，逐行建元素会把帧时间拖垮；服务端已经按字节截断过一次，这里再按
/// 行数兜一层，超出的部分只给一句提示。
pub(super) const MAX_RENDERED_LINES: usize = 2000;

/// 行号槽的宽度。
const GUTTER_WIDTH: f32 = 44.0;

/// 变更标记条的宽度。
const MARK_WIDTH: f32 = 3.0;

/// 按扩展名推断高亮语言；认不出来按纯文本。
///
/// 取值必须是 `gpui_component::highlighter::Language` 注册过的名字（它按名字找语法）。
/// 名字对应的语言特性没开启时高亮器会退化成纯文本，不会报错。
pub(super) fn language_for_path(path: &str) -> &'static str {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    match extension.to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "toml" => "toml",
        "json" | "jsonc" => "json",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "css" => "css",
        "html" | "htm" => "html",
        "py" => "python",
        "sh" | "bash" | "zsh" => "bash",
        "diff" | "patch" => "diff",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "zig" => "zig",
        _ => "text",
    }
}

/// 正文每一行的字节区间，不含行尾换行符；口径与 `str::lines` 一致（`\r\n` 一并吃掉）。
pub(super) fn line_ranges(text: &str) -> Vec<Range<usize>> {
    let bytes = text.as_bytes();
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'\n' {
            continue;
        }
        let end = if index > start && bytes[index - 1] == b'\r' {
            index - 1
        } else {
            index
        };
        ranges.push(start..end);
        start = index + 1;
    }
    if start < text.len() {
        ranges.push(start..text.len());
    }
    ranges
}

/// 高亮整份正文；返回正文字节区间上的样式表。
///
/// 同一份正文只高亮一次：按行裁剪比逐行各跑一遍语法分析便宜得多。
pub(super) fn highlight(
    text: &str,
    language: &str,
    theme: &HighlightTheme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut highlighter = SyntaxHighlighter::new(language);
    highlighter.update(None, &Rope::from(text), None);
    highlighter.styles(&(0..text.len()), theme)
}

/// 把整份正文的样式裁到一行，并把区间挪成相对该行起点。
///
/// `StyledText::with_default_highlights` 会用默认样式补齐没有样式的空隙，所以这里只负责
/// 裁剪与平移，不补空档。
pub(super) fn clip_to_line(
    line: &Range<usize>,
    styles: &[(Range<usize>, HighlightStyle)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut clipped = Vec::new();
    for (range, style) in styles {
        let start = range.start.max(line.start);
        let end = range.end.min(line.end);
        if start >= end {
            continue;
        }
        clipped.push((start - line.start..end - line.start, *style));
    }
    clipped
}

/// 渲染只读代码正文：每行一个「行号 + 变更标记 + 该行高亮文本」的横排。
///
/// 三者同处一行，因此不会错位；正文不换行，超宽时由外层横向滚动。`changes` 的下标与行序一致
/// （见 [`super::annotate::line_changes`]），比正文短的部分按「没变过」算。
pub(super) fn render_code(
    text: &str,
    language: &str,
    theme: &HighlightTheme,
    changes: &[Option<LineChange>],
    window: &Window,
    cx: &App,
) -> AnyElement {
    let styles = highlight(text, language, theme);
    let ranges = line_ranges(text);
    let rendered = ranges.len().min(MAX_RENDERED_LINES);
    // 行号按最大行号的位数补空格：等宽字体下这等价于右对齐，且不依赖文本对齐 API。
    let digits = rendered.to_string().len();

    let mut default_style = window.text_style();
    default_style.font_family = cx.theme().mono_font_family.clone();
    default_style.color = cx.theme().foreground;

    // 字号不在这里改：行号与正文必须同字号，两边都取窗口默认值就自然对齐。
    let mut column = v_flex().w_full().min_w_0().py_2();
    for (index, line) in ranges.iter().take(rendered).enumerate() {
        column = column.child(
            h_flex()
                .w_full()
                .min_w_0()
                .flex_shrink_0()
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(GUTTER_WIDTH))
                        .pr_2()
                        .whitespace_nowrap()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{:>digits$}", index + 1)),
                )
                // 没变过的行也占住这一条：少画一列会让同一份正文的代码左右参差。
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(MARK_WIDTH))
                        .mr_2()
                        .self_stretch()
                        .bg(changes
                            .get(index)
                            .copied()
                            .flatten()
                            .map_or_else(|| cx.theme().transparent, |change| change_color(change, cx))),
                )
                // 代码的缩进靠空格表达，且必须独占一行：换行会让行号与正文错位。
                .child(div().min_w_0().whitespace_nowrap().child(SelectableLine::new(
                    SharedString::from(format!("code-line-{index}")),
                    index as u64,
                    text[line.clone()].to_owned(),
                    clip_to_line(line, &styles),
                    default_style.clone(),
                ))),
        );
    }

    if ranges.len() > rendered {
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

/// 变更标记条的颜色：绿=新增、黄=改动、红=删除。
fn change_color(change: LineChange, cx: &App) -> Hsla {
    match change {
        LineChange::Added => cx.theme().success,
        LineChange::Modified => cx.theme().warning,
        LineChange::Deleted => cx.theme().danger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_follows_the_extension() {
        assert_eq!(language_for_path("src/main.rs"), "rust");
        assert_eq!(language_for_path("Cargo.toml"), "toml");
        assert_eq!(language_for_path("web/app.tsx"), "tsx");
        assert_eq!(language_for_path("scripts/run.SH"), "bash");
        assert_eq!(language_for_path("build.zig"), "zig");
        assert_eq!(language_for_path("Makefile"), "text");
        assert_eq!(language_for_path("a.unknown"), "text");
    }

    #[test]
    fn line_ranges_match_str_lines() {
        assert_eq!(line_ranges(""), Vec::<Range<usize>>::new());
        assert_eq!(line_ranges("a"), vec![0..1]);
        assert_eq!(line_ranges("a\n"), vec![0..1]);
        assert_eq!(line_ranges("a\nb"), vec![0..1, 2..3]);
        // 空行也是一行。
        assert_eq!(line_ranges("a\n\nb"), vec![0..1, 2..2, 3..4]);
        // `\r\n` 与 `lines()` 同一口径：回车不算进行内容。
        assert_eq!(line_ranges("a\r\nb\r\n"), vec![0..1, 3..4]);
    }

    #[test]
    fn clip_to_line_rebases_and_trims() {
        let styles = vec![
            (0..5, HighlightStyle::default()),
            (8..14, HighlightStyle::default()),
            (20..24, HighlightStyle::default()),
        ];
        // 第二行的区间是 7..14，只有 8..14 落在里面，且平移成 1..7。
        assert_eq!(
            clip_to_line(&(7..14), &styles),
            vec![(1..7, HighlightStyle::default())]
        );
        // 完全落在行外的样式被丢掉。
        assert_eq!(clip_to_line(&(14..20), &styles), Vec::new());
    }

    #[test]
    fn rust_code_gets_real_highlighting() {
        let theme = HighlightTheme::default_dark();
        let styles = highlight("fn main() {}\n", "rust", theme.as_ref());
        assert!(!styles.is_empty());
        // 纯文本没有任何语法样式，整段只有一个默认区间。
        let plain = highlight("fn main() {}\n", "text", theme.as_ref());
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].1, HighlightStyle::default());
    }

    /// zig 的语言特性没开时高亮器会退化成纯文本，这条正是那个失败模式的哨兵。
    #[test]
    fn zig_code_gets_real_highlighting() {
        let theme = HighlightTheme::default_dark();
        let styles = highlight("pub fn main() void {}\n", "zig", theme.as_ref());
        assert!(styles.len() > 1, "zig 语法没生效：只拿到 {styles:?}");
    }
}
