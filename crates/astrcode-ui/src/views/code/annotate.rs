//! 正文行的变更标注：把统一 diff 折成「当前文件的每一行相对 HEAD 是什么状态」。
//!
//! 正文栏显示的是**工作区当前的内容**，被删掉的行在正文里根本不存在，因此三种状态的口径是：
//!
//! - [`LineChange::Added`]：这一行是新增的（所在段落只有增、没有删）。
//! - [`LineChange::Modified`]：这一行所在段落既有删又有增，也就是这一行替换掉了原来的内容。
//! - [`LineChange::Deleted`]：这一行之前原本还有几行被删掉了，正文里看不到它们，标记落在删除
//!   位置之后的那一行上；删除发生在文件末尾时落在最后一行上。
//!
//! 解析只在 hunk 内部认 `+`/`-`：hunk 之外的 `---`/`+++` 是文件名行，不是增删。

/// 一行的变更状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LineChange {
    /// 新增的行。
    Added,
    /// 被改动过的行（所在段落有删有增）。
    Modified,
    /// 删除位置之后的那一行：它在正文里是「这里少了内容」的锚点。
    Deleted,
}

/// 按当前文件的行序给出标注；下标是行号（从 0 起），`None` 表示这一行相对 HEAD 没变。
///
/// `line_count` 是正文的总行数，返回的表长度与它一致。`unified_diff` 里出现、但超出
/// `line_count` 的行号（服务端截断过正文时会有）直接落到表的长度上。
pub(super) fn line_changes(unified_diff: &str, line_count: usize) -> Vec<Option<LineChange>> {
    let mut marks = vec![None; line_count];
    let mut hunk: Option<(usize, Vec<&str>)> = None;

    for raw in unified_diff.lines() {
        if let Some(new_start) = hunk_start(raw) {
            if let Some((start, body)) = hunk.take() {
                apply_hunk(start, &body, &mut marks);
            }
            hunk = Some((new_start, Vec::new()));
            continue;
        }
        if let Some((_, body)) = hunk.as_mut() {
            // `\ No newline at end of file` 不属于任何一边，读它只会让行号错位。
            if !raw.starts_with('\\') {
                body.push(raw);
            }
        }
    }
    if let Some((start, body)) = hunk {
        apply_hunk(start, &body, &mut marks);
    }
    marks
}

/// 解析 `@@ -旧起点,旧行数 +新起点,新行数 @@`，返回新文件里的起始行号（1 起）。
///
/// 行数可以省略（`@@ -1 +1 @@`），省略时按 1 行算；这里只用到 `+` 那半。
fn hunk_start(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("@@ -")?;
    let (_, rest) = rest.split_once('+')?;
    let number = rest
        .split([',', ' '])
        .next()
        .filter(|value| !value.is_empty())?;
    number.parse().ok()
}

fn apply_hunk(start: usize, body: &[&str], marks: &mut [Option<LineChange>]) {
    let mut line = start;
    let mut index = 0;
    while index < body.len() {
        let raw = body[index];
        if raw.starts_with('-') || raw.starts_with('+') {
            // 连续的增删算一段：只有增是「新增」，有删有增是「改动」，只有删是「删除」。
            let mut deleted = 0;
            let mut added = 0;
            while index < body.len()
                && (body[index].starts_with('-') || body[index].starts_with('+'))
            {
                if body[index].starts_with('-') {
                    deleted += 1;
                } else {
                    added += 1;
                }
                index += 1;
            }
            let change = match (deleted, added) {
                (0, _) => LineChange::Added,
                (_, 0) => LineChange::Deleted,
                _ => LineChange::Modified,
            };
            if added == 0 {
                // 删除位置之后的那一行是锚点：文件末尾的删除落在最后一行上。
                if let Some(slot) = anchor(line, marks.len()) {
                    marks[slot] = Some(LineChange::Deleted);
                }
            } else {
                for offset in 0..added {
                    if let Some(slot) = anchor(line + offset, marks.len()) {
                        marks[slot] = Some(change);
                    }
                }
            }
            line += added;
            continue;
        }
        if raw.starts_with(' ') {
            line += 1;
        }
        index += 1;
    }
}

/// 把 1 起的行号折成表下标；超出正文的行号贴到最后一行上。
fn anchor(line: usize, line_count: usize) -> Option<usize> {
    if line_count == 0 {
        return None;
    }
    Some(line.saturating_sub(1).min(line_count - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pure_addition_marks_only_the_new_lines() {
        let diff = "@@ -1,2 +1,3 @@\n a\n+b\n c\n";
        assert_eq!(
            line_changes(diff, 3),
            vec![None, Some(LineChange::Added), None]
        );
    }

    #[test]
    fn a_replacement_reads_as_a_modification() {
        let diff = "@@ -1,2 +1,2 @@\n a\n-b\n+B\n";
        assert_eq!(
            line_changes(diff, 2),
            vec![None, Some(LineChange::Modified)]
        );
    }

    #[test]
    fn a_pure_deletion_lands_on_the_following_line() {
        let diff = "@@ -1,3 +1,2 @@\n a\n-b\n c\n";
        assert_eq!(line_changes(diff, 2), vec![None, Some(LineChange::Deleted)]);
    }

    #[test]
    fn a_deletion_at_the_end_of_file_lands_on_the_last_line() {
        let diff = "@@ -1,2 +1,1 @@\n a\n-b\n";
        assert_eq!(line_changes(diff, 2), vec![None, Some(LineChange::Deleted)]);
    }

    #[test]
    fn several_hunks_keep_their_own_line_numbers() {
        let diff = "@@ -1,1 +1,2 @@\n a\n+b\n@@ -10,1 +11,1 @@\n-o\n+O\n";
        let marks = line_changes(diff, 12);
        assert_eq!(marks[1], Some(LineChange::Added));
        assert_eq!(marks[10], Some(LineChange::Modified));
    }

    #[test]
    fn file_headers_outside_a_hunk_are_not_changes() {
        let diff = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+A\n";
        assert_eq!(
            line_changes(diff, 1),
            vec![Some(LineChange::Modified)],
            "文件名行不能被当成删除与新增"
        );
    }

    #[test]
    fn a_hunk_without_counts_counts_one_line() {
        assert_eq!(hunk_start("@@ -1 +7 @@"), Some(7));
        assert_eq!(hunk_start("@@ -1,3 +7,4 @@ fn f() {"), Some(7));
        assert_eq!(hunk_start("diff --git a/x b/x"), None);
    }

    #[test]
    fn lines_past_the_end_of_the_rendered_document_clamp_to_the_last_one() {
        let diff = "@@ -1,1 +1,2 @@\n a\n+b\n";
        assert_eq!(line_changes(diff, 2), vec![None, Some(LineChange::Added)]);
        assert_eq!(line_changes(diff, 1), vec![Some(LineChange::Added)]);
    }
}
