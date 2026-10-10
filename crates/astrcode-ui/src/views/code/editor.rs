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
    AnyElement, App, FocusHandle, HighlightStyle, Hsla, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window,
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
pub(super) const GUTTER_WIDTH: f32 = 44.0;

/// 变更标记条的宽度。
const MARK_WIDTH: f32 = 3.0;

/// 正文栏的纵向内边距：上下各留这么多，滚到顶、滚到底时不贴边。
///
/// 换行号与像素的那份换算要用同一个值（见 [`visible_rows`]），否则滚动位置会慢慢错开。
pub(super) const VERTICAL_PADDING: f32 = 8.0;

/// 可视区上下各多渲染的行数。
///
/// 行高按 [`visible_rows`] 估，与真实排版的行高可能差零点几像素；两侧各留几行，这点误差就落在
/// 可视区之外，上下滚动时不会先在边缘露白。
const OVERSCAN_ROWS: usize = 8;

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

/// 当前该渲染的行区间：由滚动偏移、视口高度、行高与总行数算出，两端各留 [`OVERSCAN_ROWS`] 行。
///
/// `offset_y` 收框架 `ScrollHandle` 的原始值：它向下是负数（滚过去多少像素即 `-offset_y`），
/// 这里统一取反。偏移量可能还停在上一份正文的末尾（切文件时滚动位置不会自己回去），所以先按这份
/// 正文的内容高度夹一次再算区间；视口高度未知时按 0 处理，只会多画几行。
pub(super) fn visible_rows(
    offset_y: f32,
    viewport_height: f32,
    line_height: f32,
    total: usize,
) -> Range<usize> {
    if total == 0 || line_height <= 0.0 {
        return 0..0;
    }
    let content_height = line_height * total as f32 + VERTICAL_PADDING * 2.0;
    let scrolled = (-offset_y).clamp(0.0, (content_height - viewport_height).max(0.0));
    let first = (((scrolled - VERTICAL_PADDING).max(0.0) / line_height) as usize).min(total - 1);
    let visible = (viewport_height / line_height).ceil() as usize + 1;
    first.saturating_sub(OVERSCAN_ROWS)..(first + visible + OVERSCAN_ROWS).min(total)
}

/// 一行上的两层着色：语法高亮在前，查找命中在后，重叠处让命中盖住语法色。
///
/// 两层都按**整份正文**给，逐行裁剪在 [`render_code`] 里做：整份只跑一次语法分析，比逐行各跑
/// 一遍便宜得多。
pub(super) struct LineLayers<'a> {
    pub(super) syntax: &'a [(Range<usize>, HighlightStyle)],
    pub(super) find: &'a [(Range<usize>, HighlightStyle)],
}

/// 渲染只读代码正文：每行一个「行号 + 变更标记 + 该行高亮文本」的横排。
///
/// 三者同处一行，因此不会错位；正文不换行，超宽时由外层横向滚动。`changes` 的下标与行序一致
/// （见 [`super::annotate::line_changes`]），比正文短的部分按「没变过」算。
///
/// 只渲染 `rows` 这一段：整份正文可能有几十万行，全铺出来会把帧时间拖垮（见 [`visible_rows`]）。
/// 没渲染的行由上下两段留白占住高度，滚动条的量程因此仍与整份正文一致。
pub(super) fn render_code(
    text: &str,
    lines: &[Range<usize>],
    layers: LineLayers<'_>,
    changes: &[Option<LineChange>],
    rows: Range<usize>,
    window: &Window,
    cx: &App,
) -> AnyElement {
    let rendered = lines.len().min(MAX_RENDERED_LINES);
    let line_height = window.line_height();
    // 调用方按同一个总行数算的区间，这里再夹一次：越界下标取行文本会直接 panic。
    let rows = rows.start.min(rendered)..rows.end.min(rendered);

    // 行号按最大行号的位数补空格：等宽字体下这等价于右对齐，且不依赖文本对齐 API。
    let digits = rendered.to_string().len();

    let mut default_style = window.text_style();
    default_style.font_family = cx.theme().mono_font_family.clone();
    default_style.color = cx.theme().foreground;

    // 字号不在这里改：行号与正文必须同字号，两边都取窗口默认值就自然对齐。
    let mut column = v_flex().w_full().min_w_0().child(vertical_space(
        px(VERTICAL_PADDING) + line_height * rows.start as f32,
    ));
    for (index, line) in lines[rows.clone()].iter().enumerate() {
        let number = rows.start + index;
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
                        .child(format!("{:>digits$}", number + 1)),
                )
                // 没变过的行也占住这一条：少画一列会让同一份正文的代码左右参差。
                .child(
                    div()
                        .flex_shrink_0()
                        .w(px(MARK_WIDTH))
                        .mr_2()
                        .self_stretch()
                        .bg(changes
                            .get(number)
                            .copied()
                            .flatten()
                            .map_or_else(|| cx.theme().transparent, |change| change_color(change, cx))),
                )
                // 代码的缩进靠空格表达，且必须独占一行：换行会让行号与正文错位。
                .child(div().min_w_0().whitespace_nowrap().child(SelectableLine::new(
                    SharedString::from(format!("code-line-{number}")),
                    number as u64,
                    text[line.clone()].to_owned(),
                    line_highlights(line, &layers),
                    default_style.clone(),
                ))),
        );
    }

    column = column.child(vertical_space(
        line_height * (rendered - rows.end) as f32 + px(VERTICAL_PADDING),
    ));

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

/// 一行的样式：两层各自裁到这一行，再叠成一层——查找命中盖住语法色。
///
/// 交给 `StyledText` 的这一层必须按起点有序、互不重叠：它按「每条 run 依次吃掉一段文本」推出
/// 样式，重叠会让它算出比正文更长的 run，直接 panic（`invalid text run`）。两层各自有序还不够
/// ——命中通常落在某个 token 内部，首尾相接必然重叠，因此这里按两层的边界把行切成小段再逐段
/// 选样式（见 [`stack_layers`]）。
fn line_highlights(
    line: &Range<usize>,
    layers: &LineLayers<'_>,
) -> Vec<(Range<usize>, HighlightStyle)> {
    stack_layers(
        &clip_to_line(line, layers.syntax),
        &clip_to_line(line, layers.find),
    )
}

/// 把两层样式叠成一层：`over` 盖住 `under`，结果按起点有序、互不重叠。
///
/// 两层各自有序且互不重叠（前者来自 [`clip_to_line`]，后者来自
/// [`crate::find::literal_matches`]），因此各走一个游标就够：取两层剩下的边界里最近的那个，
/// 把行切成小段，每段取盖在上面那层的样式，没有就取下面那层。
fn stack_layers(
    under: &[(Range<usize>, HighlightStyle)],
    over: &[(Range<usize>, HighlightStyle)],
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut stacked: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut under_ix = 0;
    let mut over_ix = 0;
    let mut cursor = 0;
    loop {
        while under
            .get(under_ix)
            .is_some_and(|(range, _)| range.end <= cursor)
        {
            under_ix += 1;
        }
        while over
            .get(over_ix)
            .is_some_and(|(range, _)| range.end <= cursor)
        {
            over_ix += 1;
        }
        // 当前区间之前已经过去的部分不再贡献边界，只剩下它的终点。
        let next = [under.get(under_ix), over.get(over_ix)]
            .into_iter()
            .flatten()
            .flat_map(|(range, _)| [range.start, range.end])
            .filter(|position| *position > cursor)
            .min();
        let Some(end) = next else {
            break;
        };
        let style = style_at(over, over_ix, cursor).or_else(|| style_at(under, under_ix, cursor));
        if let Some(style) = style {
            // 被盖住那层的边界不该把上面的色切成几段：相邻同色的段并成一段。
            match stacked.last_mut() {
                Some((range, last)) if *last == style && range.end == cursor => range.end = end,
                _ => stacked.push((cursor..end, style)),
            }
        }
        cursor = end;
    }
    stacked
}

/// `layer` 从 `index` 起的那条区间盖住 `position` 时的样式；没有盖住就是 `None`。
fn style_at(
    layer: &[(Range<usize>, HighlightStyle)],
    index: usize,
    position: usize,
) -> Option<HighlightStyle> {
    layer
        .get(index)
        .filter(|(range, _)| range.start <= position && position < range.end)
        .map(|(_, style)| *style)
}


/// 正文栏的滚动容器：两个方向都能滚、能拿焦点，里面的行参与窗口级文本选择。
///
/// 焦点不是装饰，是复制能不能用的前提：Ctrl+C 由窗口根节点（`gpui_kit::base::Root`）的 `Copy`
/// 处理，按键要先被派发到某个节点，才有机会沿路径冒泡到根；一个焦点都没有时，按键的派发路径
/// 只有派发树的根，那里既没有 `Root` 键上下文也没有 `Copy` 的监听者，按下去毫无反应。按下鼠标
/// 时把焦点收到这一栏，路径才接得上。
pub(super) fn pane_scroll(
    focus: &FocusHandle,
    scroll: &ScrollHandle,
    body: AnyElement,
) -> AnyElement {
    let focus_on_click = focus.clone();
    div()
        .id("code-pane-scroll")
        .flex_1()
        .min_h_0()
        .w_full()
        .overflow_scroll()
        .track_scroll(scroll)
        .track_focus(focus)
        .focusable()
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            window.focus(&focus_on_click, cx)
        })
        .child(body)
        .into_any_element()
}

/// 一段纵向留白：替没有渲染的行占住高度。
///
/// 变更栏的双栏也用同一段留白（见 [`super::diff::render_aligned`]），两栏因此共用同一份
/// 「按行高换算滚动位置」的口径。
pub(super) fn vertical_space(height: Pixels) -> AnyElement {
    div().flex_shrink_0().h(height).into_any_element()
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
    use gpui_kit::{
        AppContext as _, Context, Modifiers, MouseButton, Render, ScrollHandle, TestAppContext,
        base::TextSelectionLayer, point, prelude::FluentBuilder as _,
    };

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

    /// 可视区间要覆盖视口内的行，并夹在正文两端之内。
    ///
    /// 这里算错的下场是「滚到某处突然整屏空白」，所以两端各钉一条：滚到顶时从第 0 行开始、
    /// 滚到底时最后一行必须在区间里。
    #[test]
    fn visible_rows_cover_the_viewport_and_stay_in_range() {
        // 20 行、行高 20px、视口 100px：视口内 5 行，两端各留缓冲。
        let rows = visible_rows(0.0, 100.0, 20.0, 20);
        assert_eq!(rows.start, 0);
        assert!(rows.end >= 6, "应覆盖视口内的 5 行：{rows:?}");

        // 滚到底：内容高 416（20×20 加上下内边距）减视口 100，偏移就是 -316。
        let rows = visible_rows(-316.0, 100.0, 20.0, 20);
        assert_eq!(rows.end, 20);
        assert!(rows.start <= 15, "滚到底时第 15 行应已渲染：{rows:?}");

        // 偏移指向上一份更长的正文时应按当前内容高度夹住，而不是算出空区间。
        let rows = visible_rows(-100_000.0, 100.0, 20.0, 20);
        assert_eq!(rows.end, 20);
        assert!(rows.start <= 15, "偏移越界时也要落在正文末尾：{rows:?}");

        // 空正文没有行可渲染。
        assert_eq!(visible_rows(0.0, 100.0, 20.0, 0), 0..0);
    }

    /// 虚拟化不能改变滚动条量程：没渲染的行由上下留白占住，量程仍与整份正文一致。
    ///
    /// 量程由框架在布局时量出（`max_offset` 加视口高），因此正文行数要多于视口能装下的行，否则
    /// 量到的是视口高度而不是内容高度。这条也钉住留白公式：留白按 `window.line_height()` 算、
    /// 真实行高由排版器定，两者差零点几像素的话 100 行就能差出好几像素。
    #[gpui_kit::test]
    fn spacers_keep_the_scroll_range_of_the_whole_body(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        let text: String = (0..100)
            .map(|index| format!("let value_{index} = compute_{index}();\n"))
            .collect();
        let lines = line_ranges(&text);
        let theme = HighlightTheme::default_dark();
        let styles = highlight(&text, "rust", theme.as_ref());
        let marks = vec![None; lines.len()];

        // 内容高度 = 滚动量程 + 视口高度，两者都由框架在布局时量好。
        let probe = |cx: &mut TestAppContext, rows: Range<usize>| {
            let scroll = ScrollHandle::new();
            let (_, cx) = cx.add_window_view(|_, cx| ScrollProbe {
                text: text.clone(),
                lines: lines.clone(),
                styles: styles.clone(),
                marks: marks.clone(),
                rows,
                focus: cx.focus_handle(),
                layer: true,
                scroll: scroll.clone(),
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

        let (full, line_height) = probe(cx, 0..lines.len());
        let (windowed, _) = probe(cx, 0..8);
        let expected = line_height * lines.len() as f32 + 2.0 * VERTICAL_PADDING;
        assert!(
            (full - expected).abs() <= 1.0,
            "整份渲染的内容高度 {full} 与「行高 × 行数 + 上下内边距」{expected} 不一致"
        );
        assert!(
            (full - windowed).abs() <= 1.0,
            "只渲染 8 行时的滚动量程应仍与整份正文一致：全量 {full}，窗口 {windowed}"
        );
    }

    /// 正文栏接上焦点之后，Ctrl+C 才把选中的行写进剪贴板。
    ///
    /// 复制不由正文自己做：按键由窗口根节点（`base::Root`）接住，读的是窗口级选择层里各参与者的
    /// 副本，而按键得先有焦点路径才走得到根——一个焦点都没有时，按键的派发路径只有派发树的根，
    /// 那里既没有 `Root` 键上下文也没有 `Copy` 的监听者，按下去毫无反应。这条测试因此走整条线：
    /// 真的根节点 + 真的正文栏容器（[`pane_scroll`]）+ 「按下鼠标→拿焦点」，并且正文是被虚拟化
    /// 过的（只渲染前 8 行）。
    #[gpui_kit::test]
    fn ctrl_c_copies_the_selection_of_a_focused_pane(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        let text: String = (0..20)
            .map(|index| format!("let value_{index} = compute_{index}();\n"))
            .collect();
        let lines = line_ranges(&text);
        let theme = HighlightTheme::default_dark();
        let styles = highlight(&text, "rust", theme.as_ref());

        let (_, cx) = cx.add_window_view(|window, cx| {
            let probe = cx.new(|cx| ScrollProbe {
                text: text.clone(),
                lines: lines.clone(),
                styles: styles.clone(),
                marks: vec![None; lines.len()],
                rows: 0..8,
                focus: cx.focus_handle(),
                layer: false,
                scroll: ScrollHandle::new(),
            });
            gpui_kit::base::Root::new(probe, window, cx)
        });

        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        // 行在正文栏里的纵向位置：上方留白 + 行号。拖动从第一行跨到第二行。
        let line_height = cx.update(|window, _| f32::from(window.line_height()));
        let first = VERTICAL_PADDING + line_height * 0.5;
        let second = VERTICAL_PADDING + line_height * 1.5;
        cx.simulate_mouse_down(
            point(px(60.), px(first)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_move(
            point(px(200.), px(second)),
            Some(MouseButton::Left),
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            point(px(200.), px(second)),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.simulate_keystrokes("ctrl-c");
        let copied = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        assert!(
            copied.contains("value_0") && copied.contains("value_1"),
            "Ctrl+C 该把选中的两行写进剪贴板，实际复制到 {copied:?}"
        );
    }

    /// 叠起来的样式层必须有序、互不重叠，且命中盖住语法色。
    ///
    /// 交给 `StyledText` 的那一层一旦重叠，它按 run 长度推样式时会算出比正文更长的 run 并直接
    /// panic（`invalid text run`）——正文里搜一个词就崩，正是这个不变式被破坏后的样子。
    ///
    /// 测试用的样式只给一层底色，好看出叠出来的每一段归哪一层。
    fn style(color: Hsla) -> HighlightStyle {
        HighlightStyle {
            background_color: Some(color),
            ..Default::default()
        }
    }

    #[test]
    fn stacked_layers_stay_sorted_and_disjoint() {
        let under = vec![(0..5, style(Hsla::red())), (7..10, style(Hsla::red()))];
        // 命中落在语法区间内部：切成三段，中间那段归命中。
        assert_eq!(
            stack_layers(&under, &[(2..4, style(Hsla::blue()))]),
            vec![
                (0..2, style(Hsla::red())),
                (2..4, style(Hsla::blue())),
                (4..5, style(Hsla::red())),
                (7..10, style(Hsla::red())),
            ]
        );

        // 命中横跨语法区间的边界时，两侧各留一段语法色，中间的空隙也归命中。
        assert_eq!(
            stack_layers(&under, &[(3..8, style(Hsla::blue()))]),
            vec![
                (0..3, style(Hsla::red())),
                (3..8, style(Hsla::blue())),
                (8..10, style(Hsla::red())),
            ]
        );

        // 两层都盖满整行时，叠出来的长度必须正好等于正文长度。
        let stacked = stack_layers(
            &[(0..6, style(Hsla::red()))],
            &[(1..6, style(Hsla::blue()))],
        );
        assert_eq!(
            stacked.iter().map(|(range, _)| range.len()).sum::<usize>(),
            6
        );
    }

    /// 命中落在语法 token 里时，那一行仍要画得出来。
    ///
    /// 这条走的正是崩溃现场：正文栏把语法层与命中层叠起来交给 `StyledText`，两层一重叠就在框架
    /// 里 panic。手写两层而不是跑 tree-sitter，是为了让重叠位置是确定的。
    #[gpui_kit::test]
    fn a_find_hit_inside_a_syntax_token_still_draws_the_line(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            crate::theme::install(cx);
        });
        let syntax = vec![(0..5, style(Hsla::red()))];
        let find = vec![(2..4, style(Hsla::blue()))];

        let (_, cx) = cx.add_window_view(move |_, _| OverlapProbe { syntax, find });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// 一行的两层样式（语法与命中），叠好之后交给 [`SelectableLine`]。
    struct OverlapProbe {
        syntax: Vec<(Range<usize>, HighlightStyle)>,
        find: Vec<(Range<usize>, HighlightStyle)>,
    }

    impl Render for OverlapProbe {
        fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let text = "abcdefgh";
            div().size_full().child(SelectableLine::new(
                "overlap-line",
                0,
                text,
                line_highlights(
                    &(0..text.len()),
                    &LineLayers {
                        syntax: &self.syntax,
                        find: &self.find,
                    },
                ),
                window.text_style(),
            ))
        }
    }


    /// 量滚动量程与测复制用的最小正文栏：一个能拿焦点的滚动容器加一份被虚拟化的正文。
    ///
    /// `layer` 为假时不自带选择层：那一层由窗口根节点（`base::Root`）提供，叠两层会掩盖「根节点
    /// 那一层是否真的接到选择」这件事。
    struct ScrollProbe {
        text: String,
        lines: Vec<Range<usize>>,
        styles: Vec<(Range<usize>, HighlightStyle)>,
        marks: Vec<Option<LineChange>>,
        rows: Range<usize>,
        focus: FocusHandle,
        layer: bool,
        scroll: ScrollHandle,
    }

    impl Render for ScrollProbe {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .when(self.layer, |this| this.child(TextSelectionLayer))
                .child(pane_scroll(
                    &self.focus,
                    &self.scroll,
                    render_code(
                        &self.text,
                        &self.lines,
                        // 这一探针只量滚动与复制，不带查找高亮。
                        LineLayers {
                            syntax: &self.styles,
                            find: &[],
                        },
                        &self.marks,
                        self.rows.clone(),
                        window,
                        cx,
                    ),
                ))
        }
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
