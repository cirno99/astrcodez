//! 跨宿主与扩展共用的文本截断原语。
//!
//! 这里的每个函数都把**预算单位**写进名字(字节 / 字符)。历史上同一段「收字符边界再切」
//! 的循环被抄了十几处,单位与「标记是否计入预算」各自为政,于是出现了按字节切中文、
//! 或者结果超出调用方给定预算的缺陷。新增截断逻辑前先在这里找语义匹配的函数。
//!
//! token 预算不在这里:token 是按模型估算的,估算器归 `astrcode-context`。
//!
//! 不变式:(1) 任何返回都是合法 UTF-8,绝不切在多字节字符中间;
//! (2) 结果长度不超过调用方给定的预算(带标记的变体保证「前缀 + 标记」也不超)。

/// 不大于 `index` 的最大字符边界,越界时收敛到 `text.len()`。
///
/// `str::floor_char_boundary` 至今仍是 nightly 不稳定 API,本仓库不启用 `#![feature]`,
/// 因此这里手写。
pub fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 不小于 `index` 的最小字符边界,越界时收敛到 `text.len()`。
pub fn ceil_char_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// 取不超过 `max_bytes` 字节的头部,切点收在字符边界上。
pub fn truncate_bytes_head(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    &text[..floor_char_boundary(text, max_bytes)]
}

/// 取不超过 `max_bytes` 字节的尾部,切点收在字符边界上;第二项表示是否发生了截断。
///
/// 返回的是 `&str` 切片而非 lossy 解码结果:调用方若拿到 `&[u8]` 再解码,
/// 落在字符中间的窗口会变成 U+FFFD,那是内容损坏而不是截断。
pub fn truncate_bytes_tail(text: &str, max_bytes: usize) -> (&str, bool) {
    if text.len() <= max_bytes {
        return (text, false);
    }
    let start = ceil_char_boundary(text, text.len() - max_bytes);
    (&text[start..], true)
}

/// 取不超过 `max_chars` 个字符,第二项表示是否发生了截断。
pub fn truncate_chars(text: &str, max_chars: usize) -> (String, bool) {
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    let was_truncated = chars.next().is_some();
    (truncated, was_truncated)
}

/// 取不超过 `max_chars` 个字符,并在截断时追加 `marker`;标记自身的字符数计入预算。
///
/// 文本整体装得下时原样返回(不追加标记)。预算装不下整个标记时退化为硬截断,
/// 以便「结果不超过 `max_chars`」这条不变式恒成立。
pub fn truncate_chars_with_marker(text: &str, max_chars: usize, marker: &str) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }

    let marker_chars = marker.chars().count();
    if max_chars <= marker_chars {
        return text.chars().take(max_chars).collect();
    }

    let (prefix, _) = truncate_chars(text, max_chars - marker_chars);
    format!("{prefix}{marker}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floor_and_ceil_boundaries_never_split_a_character() {
        // "你" 占 3 字节,索引 1..=2 落在字符中间。
        assert_eq!(floor_char_boundary("你好", 0), 0);
        assert_eq!(floor_char_boundary("你好", 1), 0);
        assert_eq!(floor_char_boundary("你好", 3), 3);
        assert_eq!(ceil_char_boundary("你好", 1), 3);
        assert_eq!(ceil_char_boundary("你好", 3), 3);
        assert_eq!(ceil_char_boundary("你好", 6), 6);
    }

    #[test]
    fn boundaries_clamp_past_the_end() {
        assert_eq!(floor_char_boundary("你好", 999), 6);
        assert_eq!(ceil_char_boundary("你好", 999), 6);
    }

    #[test]
    fn byte_head_keeps_the_budget_and_stays_on_a_boundary() {
        assert_eq!(truncate_bytes_head("你好 world", 4), "你");
        assert_eq!(truncate_bytes_head("你好", 100), "你好");
        assert_eq!(truncate_bytes_head("你好", 0), "");
        assert_eq!(truncate_bytes_head("abcdef", 3), "abc");
    }

    #[test]
    fn byte_tail_never_emits_a_replacement_character() {
        // 12 字节尾部取 4 字节:切点 8 落在 "界" 中间,ceil 收到 9,
        // 于是保留 3 字节的 "界"。宁可少几个字节,也不切坏字符。
        let (tail, truncated) = truncate_bytes_tail("你好世界", 4);
        assert!(truncated);
        assert_eq!(tail, "界");
        assert!(tail.len() <= 4);
        assert!(!tail.contains('\u{FFFD}'));

        let (whole, truncated) = truncate_bytes_tail("你好", 100);
        assert!(!truncated);
        assert_eq!(whole, "你好");
    }

    #[test]
    fn char_truncation_reports_whether_it_cut() {
        assert_eq!(truncate_chars("你好世界", 2), ("你好".to_owned(), true));
        assert_eq!(truncate_chars("你好", 2), ("你好".to_owned(), false));
        assert_eq!(truncate_chars("你好", 0), (String::new(), true));
    }

    #[test]
    fn marker_is_counted_inside_the_budget() {
        let marker = "...";
        let result = truncate_chars_with_marker("你好世界abc", 6, marker);
        assert_eq!(result, "你好世...");
        assert_eq!(result.chars().count(), 6);
    }

    #[test]
    fn text_that_fits_is_returned_without_a_marker() {
        assert_eq!(truncate_chars_with_marker("你好", 10, "..."), "你好");
    }

    #[test]
    fn budget_smaller_than_marker_degrades_to_a_hard_cut() {
        let marker = "[... truncated]";
        let result = truncate_chars_with_marker("你好世界abcde", 4, marker);
        assert_eq!(result, "你好世界");
        assert_eq!(result.chars().count(), 4);
    }
}
