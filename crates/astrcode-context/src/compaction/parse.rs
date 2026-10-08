//! Compact 输出解析与 contract 校验。
//!
//! 模型必须返回一个 `<summary>...</summary>` 块，并包含固定九段标题。
//! 这里不理解每段内容的语义，只守住结构 contract；摘要质量由 prompt 约束。

pub(super) const REQUIRED_SUMMARY_SECTIONS: [&str; 9] = [
    "1. Primary Request and Intent:",
    "2. Key Technical Concepts:",
    "3. Files and Code Sections:",
    "4. Errors and fixes:",
    "5. Problem Solving:",
    "6. All user messages:",
    "7. Pending Tasks:",
    "8. Current Work:",
    "9. Optional Next Step:",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ParsedCompactOutput {
    /// 已去掉外层 `<summary>` 标签的摘要正文。
    pub(super) summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CompactParseError {
    detail: String,
}

impl CompactParseError {
    pub(super) fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for CompactParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for CompactParseError {}

/// 解析 provider/runner 返回的 compact 文本。
///
/// 为了容忍模型偶尔包一层 markdown fence，这里会先剥掉最外层 fence，
/// 但不会接受缺失 `<summary>` 或缺少九段标题的输出。
pub(super) fn parse_compact_output(
    content: &str,
) -> Result<ParsedCompactOutput, CompactParseError> {
    let normalized = strip_outer_markdown_code_fence(content);
    let summary = extract_xml_block(&normalized, "summary")?
        .ok_or_else(|| CompactParseError::new("compact response missing <summary> block"))?;

    if summary.trim().is_empty() {
        return Err(CompactParseError::new("compact summary response was empty"));
    }
    if let Some(missing_section) = REQUIRED_SUMMARY_SECTIONS
        .iter()
        .find(|section| !summary.contains(**section))
    {
        return Err(CompactParseError::new(format!(
            "compact summary missing required section title: {missing_section}"
        )));
    }

    Ok(ParsedCompactOutput { summary })
}

/// 从任意 compact-ish 文本里提取可放回上下文的摘要。
///
/// 这是 assembler 的宽容路径：用于格式化已有 summary 或 deterministic fallback，
/// 不用于 provider-backed compact 的严格 contract 判断。
pub(super) fn extract_summary_for_context(content: &str) -> String {
    let normalized = strip_outer_markdown_code_fence(content);
    if let Ok(Some(summary)) = extract_xml_block(&normalized, "summary") {
        return summary;
    }
    parse_compact_output(content)
        .map(|parsed| parsed.summary)
        .unwrap_or_else(|_| {
            let without_analysis =
                strip_xml_block(&normalized, "analysis").unwrap_or_else(|_| normalized.clone());
            clean_compact_fallback_text(&without_analysis)
        })
}

/// 提取指定 XML-ish 标签内容。
///
/// 这里只实现 compact contract 所需的轻量解析：大小写不敏感，允许 opening tag
/// 带空白/属性，但不尝试成为通用 XML parser。
fn extract_xml_block(content: &str, tag: &str) -> Result<Option<String>, CompactParseError> {
    let Some((open_start, open_end)) = find_opening_tag(content, tag) else {
        return Ok(None);
    };
    let Some((close_start, _close_end)) = find_closing_tag(&content[open_end..], tag) else {
        return Err(CompactParseError::new(format!(
            "compact response missing closing </{tag}> tag"
        )));
    };
    let close_start = open_end + close_start;
    if close_start < open_start {
        return Ok(None);
    }
    Ok(Some(content[open_end..close_start].trim().to_string()))
}

/// 删除指定 XML-ish 块，用于 fallback 清理 `<analysis>` 等临时内容。
fn strip_xml_block(content: &str, tag: &str) -> Result<String, CompactParseError> {
    let Some((open_start, open_end)) = find_opening_tag(content, tag) else {
        return Ok(content.trim().to_string());
    };
    let Some((_close_start, close_end)) = find_closing_tag(&content[open_end..], tag) else {
        return Err(CompactParseError::new(format!(
            "compact response missing closing </{tag}> tag"
        )));
    };
    let close_end = open_end + close_end;
    let mut stripped = String::with_capacity(content.len().saturating_sub(close_end - open_start));
    stripped.push_str(&content[..open_start]);
    stripped.push_str(&content[close_end..]);
    Ok(stripped.trim().to_string())
}

fn find_opening_tag(content: &str, tag: &str) -> Option<(usize, usize)> {
    let lower = content.to_ascii_lowercase();
    let needle = format!("<{tag}");
    let mut start = 0;
    while let Some(relative) = lower[start..].find(&needle) {
        let tag_start = start + relative;
        let after = lower[tag_start + needle.len()..].chars().next();
        if after.is_some_and(|ch| ch != '>' && !ch.is_ascii_whitespace()) {
            start = tag_start + needle.len();
            continue;
        }
        let tag_end = lower[tag_start..].find('>')? + tag_start + 1;
        return Some((tag_start, tag_end));
    }
    None
}

fn find_closing_tag(content: &str, tag: &str) -> Option<(usize, usize)> {
    let lower = content.to_ascii_lowercase();
    let needle = format!("</{tag}");
    let start = lower.find(&needle)?;
    let after = lower[start + needle.len()..].chars().next();
    if after.is_some_and(|ch| ch != '>' && !ch.is_ascii_whitespace()) {
        return None;
    }
    let end = lower[start..].find('>')? + start + 1;
    Some((start, end))
}

fn strip_markdown_code_fence(content: &str) -> String {
    let trimmed = content.trim();
    if !trimmed.starts_with("```") {
        return trimmed.to_string();
    }

    let mut lines = trimmed.lines();
    let Some(first_line) = lines.next() else {
        return trimmed.to_string();
    };
    if !first_line.trim_start().starts_with("```") {
        return trimmed.to_string();
    }

    let body = lines.collect::<Vec<_>>().join("\n");
    let body = body.trim_end();
    body.strip_suffix("```").unwrap_or(body).trim().to_string()
}

/// 反复剥掉外层 markdown fence，容忍模型输出 ```xml ``` 再嵌一层 fence。
fn strip_outer_markdown_code_fence(content: &str) -> String {
    let mut current = content.trim().to_string();
    loop {
        let stripped = strip_markdown_code_fence(&current);
        if stripped == current {
            return current;
        }
        current = stripped;
    }
}

/// fallback 清理：保留文本主体，去掉常见“Here is the summary”式前缀。
fn clean_compact_fallback_text(content: &str) -> String {
    let without_code_fence = strip_outer_markdown_code_fence(content);
    let lines = without_code_fence
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>();
    let first_meaningful = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let cleaned = lines
        .into_iter()
        .skip(first_meaningful)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    strip_leading_summary_preamble(&cleaned)
}

fn strip_leading_summary_preamble(content: &str) -> String {
    let mut lines = content.lines();
    let Some(first_line) = lines.next() else {
        return String::new();
    };
    if is_summary_preamble_line(first_line.trim()) {
        return lines.collect::<Vec<_>>().join("\n").trim().to_string();
    }
    content.trim().to_string()
}

fn is_summary_preamble_line(line: &str) -> bool {
    let normalized = line
        .trim_matches(|ch: char| matches!(ch, '*' | '#' | '-' | ':' | ' '))
        .trim();
    normalized.eq_ignore_ascii_case("summary")
        || normalized.eq_ignore_ascii_case("here is the summary")
        || normalized.eq_ignore_ascii_case("compact summary")
        || normalized.eq_ignore_ascii_case("here's the summary")
}

/// 第 6 段是 user 消息清单；下标与 `REQUIRED_SUMMARY_SECTIONS` 的声明顺序同源，
/// 不要在这里另写字面量。
pub(super) const USER_MESSAGES_SECTION: &str = REQUIRED_SUMMARY_SECTIONS[5];

/// 摘要第 6 段列出的用户消息条目数。
///
/// 只数最浅一层的 bullet（`- ` / `* `）或编号（`12.` / `12)`）行；更深的 bullet 是
/// 上一条的补充说明，不计入，否则靠堆子条目就能伪造覆盖。标题行本身不算条目。
pub(super) fn listed_user_message_count(summary: &str) -> usize {
    let mut indents = Vec::new();
    let mut inside_user_messages = false;
    for line in summary.lines() {
        match heading_section(line) {
            Some(section) => inside_user_messages = section == USER_MESSAGES_SECTION,
            None if inside_user_messages => {
                let trimmed = line.trim_start();
                if is_list_item(trimmed) {
                    indents.push(line.len() - trimmed.len());
                }
            },
            None => {},
        }
    }
    let Some(shallowest) = indents.iter().min().copied() else {
        return 0;
    };
    indents
        .into_iter()
        .filter(|indent| *indent == shallowest)
        .count()
}

/// 该行是否是九段契约里的某个标题；只按精确标题前缀识别，正文引用不算。
fn heading_section(line: &str) -> Option<&'static str> {
    let trimmed = line.trim_start();
    REQUIRED_SUMMARY_SECTIONS
        .iter()
        .copied()
        .find(|section| trimmed.starts_with(section))
}

fn is_list_item(trimmed: &str) -> bool {
    trimmed.starts_with("- ") || trimmed.starts_with("* ") || is_numbered_item(trimmed)
}

/// `12. xxx` / `12) xxx` 形态的编号条目。
fn is_numbered_item(line: &str) -> bool {
    let digits = line.len()
        - line
            .trim_start_matches(|ch: char| ch.is_ascii_digit())
            .len();
    if digits == 0 {
        return false;
    }
    let mut rest = line[digits..].chars();
    matches!(rest.next(), Some('.' | ')')) && matches!(rest.next(), Some(' ' | '\t'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary_with_user_messages(body: &str) -> String {
        format!(
            "1. Primary Request and Intent:\n   - start\n\n2. Key Technical Concepts:\n   - \
             x\n\n3. Files and Code Sections:\n   - a.rs\n\n4. Errors and fixes:\n   - none\n\n5. \
             Problem Solving:\n   - ok\n\n6. All user messages:\n{body}\n7. Pending Tasks:\n   - \
             review\n\n8. Current Work:\n   - writing\n\n9. Optional Next Step:\n   - none"
        )
    }

    fn assert_listed(body: &str, expected: usize) {
        assert_eq!(
            listed_user_message_count(&summary_with_user_messages(body)),
            expected
        );
    }

    #[test]
    fn listed_user_message_counts_bullets_and_numbered_items() {
        assert_listed(
            "   - \"帮我把日志接上\" — 用户要求接入日志\n   - \"继续\" — 要求继续既有工作",
            2,
        );
        assert_listed("   1. 第一条\n   2) 第二条\n   3. 第三条", 3);
    }

    #[test]
    fn listed_user_message_stops_at_the_next_section() {
        // 第 7 段之后的 bullet 不能算进用户消息清单。
        assert_listed("   - 唯一一条", 1);
    }

    #[test]
    fn listed_user_message_ignores_deeper_bullets_prose_and_placeholders() {
        assert_listed("   - 第一条\n     续写说明\n     - 补充", 1);
        assert_listed("   - 第一条\n     - 该消息的补充说明", 1);
        assert_listed("   这段散文没有列表标记", 0);
        assert_listed("   (none)", 0);
    }

    #[test]
    fn listed_user_message_returns_zero_without_the_section() {
        assert_eq!(
            listed_user_message_count("1. Primary Request and Intent:\n   - x"),
            0
        );
    }
}
