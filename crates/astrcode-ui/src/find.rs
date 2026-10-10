//! 会话区与代码正文共用的查找：字面量匹配、上一处/下一处，以及那条查找栏。
//!
//! 匹配按字面量走（不是正则），大小写可切——与服务端的全局搜索同一套口径，区别只在这里搜的是
//! 已经在内存里的文本，不必再走一趟服务端。
//!
//! 「命中是什么」由持有者决定：两边的单位不同（代码正文是原文里的字节区间，会话是「第几项 +
//! 该项里的字节区间」），因此这里只提供匹配与计数，不持有命中本身。

use std::ops::Range;

use gpui_kit::{
    AnyElement, App, Context, Entity, Focusable as _, HighlightStyle, InteractiveElement as _,
    IntoElement, ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window,
    component::{
        ActiveTheme as _, Size,
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputState},
    },
    div, px, radians,
};

use crate::icons::IconName;

/// 一处命中的字节区间（相对被搜索的那段文本）。
pub(crate) type Match = Range<usize>;

/// 查找栏里查询框的宽度。
const FIND_INPUT_WIDTH: f32 = 200.0;
/// 命中计数那一格的宽度：它随数字变宽，不给固定宽度的话两侧的按钮会跟着文字挪动。
const FIND_COUNTER_WIDTH: f32 = 56.0;

/// 在 `haystack` 里找出 `needle` 的全部出现：不重叠，从左到右。
///
/// 空查询串给空表：它在每个位置都算命中，那个答案对界面没有意义。
pub(crate) fn literal_matches(haystack: &str, needle: &str, case_sensitive: bool) -> Vec<Match> {
    let mut matches = Vec::new();
    if needle.is_empty() {
        return matches;
    }
    let mut from = 0;
    while let Some(found) = find_from(haystack, needle, case_sensitive, from) {
        // 一处命中至少吃掉一个字节，因此 `from` 一定前进，循环会停。
        from = found.end;
        matches.push(found);
    }
    matches
}

/// 从 `from` 起找下一处 `needle`；命中区间按**原文**的字节算，界面才能直接拿它切文本。
fn find_from(haystack: &str, needle: &str, case_sensitive: bool, from: usize) -> Option<Match> {
    let tail = haystack.get(from..)?;
    if case_sensitive {
        let start = from + tail.find(needle)?;
        return Some(start..start + needle.len());
    }

    let needle: Vec<char> = needle.chars().collect();
    for (offset, _) in tail.char_indices() {
        let candidate = &tail[offset..];
        let mut consumed = 0;
        let mut compared = 0;
        let mut matched = true;
        for (expected, actual) in needle.iter().zip(candidate.chars()) {
            if !same_letter(*expected, actual) {
                matched = false;
                break;
            }
            compared += 1;
            consumed += actual.len_utf8();
        }
        // `zip` 在较短的一侧停下，候选比查询串短时它也会「比完」，所以还要比个数。
        if matched && compared == needle.len() {
            let start = from + offset;
            return Some(start..start + consumed);
        }
    }
    None
}

/// 两个字在「忽略大小写」的口径下是否相同：各取一次小写映射再逐个比。
///
/// 用 `char::to_lowercase` 的完整映射（`İ` 展开成两个字符，与 `i̇` 相等），但命中区间始终落在
/// 原文的字符边界上——映射只用来判相等，不改变原文的字节。
fn same_letter(left: char, right: char) -> bool {
    left == right || left.to_lowercase().eq(right.to_lowercase())
}

/// 从 `current` 走一格，两端绕回；没有命中时给 `None`。
pub(crate) fn step(current: usize, len: usize, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let current = current.min(len - 1);
    Some(if forward {
        (current + 1) % len
    } else {
        (current + len - 1) % len
    })
}

/// 命中处的底色：所有命中给淡一档的填充，当前那一处给强调色。
pub(crate) fn match_style(current: bool, cx: &App) -> HighlightStyle {
    HighlightStyle {
        background_color: Some(if current {
            cx.theme().primary
        } else {
            cx.theme().accent
        }),
        ..Default::default()
    }
}

/// 查找栏的状态：查询串、大小写口径，以及命中的总数与当前那一处。
pub(crate) struct Find {
    query: Entity<InputState>,
    case_sensitive: bool,
    count: usize,
    current: usize,
}

impl Find {
    pub(crate) fn new(query: Entity<InputState>) -> Self {
        Self {
            query,
            case_sensitive: false,
            count: 0,
            current: 0,
        }
    }

    /// 查询串；首尾空白不算内容——复制粘贴常带上换行与空格，拿它们去比一个也命中不了。
    pub(crate) fn needle(&self, cx: &App) -> String {
        self.query.read(cx).value().trim().to_owned()
    }

    pub(crate) fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    pub(crate) fn current(&self) -> usize {
        self.current
    }

    /// 切换大小写口径。
    pub(crate) fn toggle_case(&mut self) {
        self.case_sensitive = !self.case_sensitive;
    }

    /// 命中重算之后写回数量：当前那一处按新数量夹一次，落在界外就退到最后一处。
    pub(crate) fn set_count(&mut self, count: usize) {
        self.count = count;
        self.current = self.current.min(count.saturating_sub(1));
    }

    /// 把查找对准别处来的查询串（搜索结果的跳转）：查询串与口径一起换掉，当前回到第一处。
    pub(crate) fn seed(
        &mut self,
        needle: &str,
        case_sensitive: bool,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.query
            .update(cx, |state, cx| state.set_value(needle, window, cx));
        self.case_sensitive = case_sensitive;
        self.rewind();
    }

    /// 跳到指定的那一处；落在界外时按最后一处算。
    pub(crate) fn set_current(&mut self, current: usize) {
        self.current = current.min(self.count.saturating_sub(1));
    }

    /// 查询串换了：当前回到第一处。
    pub(crate) fn rewind(&mut self) {
        self.current = 0;
    }

    /// 跳到下一处/上一处。
    pub(crate) fn advance(&mut self, forward: bool) {
        if let Some(next) = step(self.current, self.count, forward) {
            self.current = next;
        }
    }

    /// 清空查询串（查找栏右端那枚按钮）。
    pub(crate) fn clear(&mut self, window: &mut Window, cx: &mut App) {
        self.query
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.count = 0;
        self.current = 0;
    }

    /// 把焦点交给查询框。
    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        let handle = self.query.read(cx).focus_handle(cx).clone();
        window.focus(&handle, cx);
    }
}

/// 查找栏要接的动作；持有者把自己的方法指过来。
///
/// 用函数指针而不是闭包：这几件都是「调自己某个方法」，没有要捕获的状态。
pub(crate) struct FindActions<T> {
    pub(crate) toggle_case: fn(&mut T, &mut Window, &mut Context<T>),
    pub(crate) prev: fn(&mut T, &mut Window, &mut Context<T>),
    pub(crate) next: fn(&mut T, &mut Window, &mut Context<T>),
    pub(crate) close: fn(&mut T, &mut Window, &mut Context<T>),
}

/// 一条查找栏：查询框、大小写开关、命中计数、上一处/下一处与关闭。
///
/// 不含外层的边距与分隔线：两处调用点一个把它嵌在页头里、一个摆在正文栏上方，外框各自加。
/// 查询串空着时只剩输入框——计数与几个按钮那一刻没有内容可讲。
pub(crate) fn render_find_bar<T: 'static>(
    find: &Find,
    actions: FindActions<T>,
    close_tooltip: &'static str,
    cx: &mut Context<T>,
) -> AnyElement {
    let mut bar = h_flex()
        .id("find-bar")
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .child(div().w(px(FIND_INPUT_WIDTH)).child(Input::new(&find.query)));

    if find.needle(cx).is_empty() {
        return bar.into_any_element();
    }

    let counter = if find.count == 0 {
        "无匹配".to_owned()
    } else {
        format!("{} / {}", find.current + 1, find.count)
    };
    bar = bar
        .child(render_case_toggle(
            "find-case",
            find.case_sensitive,
            actions.toggle_case,
            cx,
        ))
        .child(
            div()
                .w(px(FIND_COUNTER_WIDTH))
                .flex_shrink_0()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(counter),
        )
        .child(render_step_button(
            "find-prev",
            IconName::ChevronDown,
            true,
            "上一处",
            actions.prev,
            cx,
        ))
        .child(render_step_button(
            "find-next",
            IconName::ChevronDown,
            false,
            "下一处",
            actions.next,
            cx,
        ))
        .child(
            Button::new("find-close")
                .ghost()
                .icon(IconName::Close.element(Size::Small))
                .tooltip(close_tooltip)
                .on_click(cx.listener(move |this, _, window, cx| (actions.close)(this, window, cx)))
                .into_any_element(),
        );
    bar.into_any_element()
}

/// 大小写开关：文案就是「Aa」，选中时给一层强调底色。
///
/// 全局搜索的搜索框也用这一枚，因此 id 由调用点给：两处可能同屏，元素 id 不能重。
pub(crate) fn render_case_toggle<T: 'static>(
    id: &'static str,
    case_sensitive: bool,
    action: fn(&mut T, &mut Window, &mut Context<T>),
    cx: &mut Context<T>,
) -> AnyElement {
    let hover = cx.theme().list_hover;
    let mut toggle = div()
        .id(id)
        .flex_shrink_0()
        .px_2()
        .py_1()
        .rounded(cx.theme().radius)
        .text_xs()
        .on_click(cx.listener(move |this, _, window, cx| action(this, window, cx)))
        .child("Aa");
    // 选中态与悬停态只画一个：两个都画会在悬停时把选中态盖掉。
    toggle = if case_sensitive {
        toggle
            .bg(cx.theme().list_active)
            .text_color(cx.theme().foreground)
    } else {
        toggle
            .text_color(cx.theme().muted_foreground)
            .hover(move |this| this.bg(hover))
    };
    toggle.into_any_element()
}

/// 上一处/下一处：同一个下箭头，指上一处时转半圈。
fn render_step_button<T: 'static>(
    id: &'static str,
    icon: IconName,
    up: bool,
    tooltip: &'static str,
    action: fn(&mut T, &mut Window, &mut Context<T>),
    cx: &mut Context<T>,
) -> AnyElement {
    let icon = icon.element(Size::Small);
    let icon = if up {
        icon.rotate(radians(std::f32::consts::PI))
    } else {
        icon
    };
    Button::new(id)
        .ghost()
        .icon(icon)
        .tooltip(tooltip)
        .on_click(cx.listener(move |this, _, window, cx| action(this, window, cx)))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_are_literal_and_non_overlapping() {
        let haystack = "aaaa";
        assert_eq!(literal_matches(haystack, "aa", true), vec![0..2, 2..4]);
        // 元字符按字面量算：正则里 `a+` 会命中整串。
        assert_eq!(literal_matches("b a+ b", "a+", true), vec![2..4]);
        assert!(literal_matches(haystack, "b", true).is_empty());
        assert!(literal_matches(haystack, "", true).is_empty(), "空串不命中");
    }

    #[test]
    fn case_insensitive_matches_keep_original_offsets() {
        let haystack = "let Value = 1;";
        assert_eq!(literal_matches(haystack, "value", false), vec![4..9]);
        assert!(literal_matches(haystack, "value", true).is_empty());
        assert_eq!(literal_matches(haystack, "Value", true), vec![4..9]);

        // 折叠只用来判相等，偏移量仍落在原文上：多字节字符也不会被切开。
        let unicode = "Ärger";
        let found = literal_matches(unicode, "ärger", false);
        assert_eq!(found.len(), 1);
        assert_eq!(&unicode[found[0].clone()], "Ärger");

        // 候选比查询串短时不能算命中（`zip` 会静默停下）。
        assert!(literal_matches("ab", "abc", false).is_empty());
    }

    #[test]
    fn stepping_wraps_and_handles_no_matches() {
        assert_eq!(step(0, 3, true), Some(1));
        assert_eq!(step(2, 3, true), Some(0));
        assert_eq!(step(0, 3, false), Some(2));
        assert_eq!(step(1, 1, true), Some(0));
        assert_eq!(step(0, 0, true), None);
    }
}
