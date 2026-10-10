//! 退化重复守卫：识别模型陷入「短时间内大量重复文字」的死循环。
//!
//! 判定把流式文本切成**片段**（换行与句末标点都是边界），只看 assistant 正文与思考，
//! 不看工具参数。两条互补的判据都按「重复文字的体量」度量，不要求精确周期：
//!
//! - [`RepetitionRule::Window`]：窗口内去重片段数与平均片段长度都在极小范围，且连续两个
//!   窗口成立——短句轮转，响应早期就能拿下。
//! - [`RepetitionRule::PhrasePool`]：整段响应的重复质量、覆盖率与集中度同时越线——短语池
//!   重排。池子大到窗口判据够不到（去重片段数超过 [`MAX_DISTINCT_FRAGMENTS`]）、或者短语 长到超过
//!   [`MAX_AVG_FRAGMENT_CHARS`] 时，只有这条还能触发。
//!
//! 短语池的记账口径与三条阈值取自 dsh-loop-guard（DSH 的思考循环守护），它用同一组护栏在真实
//! 会话上标定过：只按去重数判定会漏掉「短语池重排」——这类循环没有精确周期，字符却高度集中在
//! 少量片段上。三条里只有集中度能挡住「同一段代码改前 / 改后贴两遍」：那种文本覆盖率天然趋近
//! 1.0，集中度却只有 2。
//!
//! 开销约束（流式热路径，每个增量都会走一遍）：
//!
//! - 增量只扫描新增字节：扫描游标随文本推进，不回退重扫。
//! - 窗口统计是增量的：去重数与总长度在入窗 / 出窗时维护，判定本身是 O(1)。
//! - 短语池只按 64 位哈希记账，去重片段数封顶 [`MAX_TRACKED_FRAGMENTS`]，内存不随输出长度增长。
//! - 单片段长度有上限：超过 [`MAX_PENDING_BYTES`] 未出现边界即判定为长文输出并重置统计，
//!   内存与单次扫描量都不随输出长度增长。


use std::{
    collections::{VecDeque, hash_map::Entry},
    sync::Arc,
};

use rustc_hash::{FxHashMap as HashMap, FxHasher};


/// 参与判定的最近片段数。
const WINDOW_FRAGMENTS: usize = 40;
/// 窗口内允许的最大去重片段数。
const MAX_DISTINCT_FRAGMENTS: usize = 16;
/// 窗口内允许的平均片段长度（字符）。
const MAX_AVG_FRAGMENT_CHARS: usize = 32;
/// 触发前需要连续满足条件的窗口数。
const CONSECUTIVE_HITS_REQUIRED: u32 = 2;
/// 未出现边界时最多保留的字节数。
const MAX_PENDING_BYTES: usize = 64 * 1024;

// ── 短语池判据 ────────────────────────────────────────────────────────────
// 三条阈值与 dsh-loop-guard 的默认值一致。选定后在本机 668 条真实 assistant 消息
// （正文 61 条、思考 607 条，均 ≥200 字符）上复核：零触发，最大覆盖率 0.477、
// 最大集中度 1.56——真实长推理会复用措辞，但重复质量占不到六成、词汇也没集中到 4 次。

/// 记账允许出现的最大去重片段数。
///
/// 触顶即停止记账：一段输出如果见过这么多互不相同的片段，它的词汇量已经远超任何循环短语池
/// （后者只有几十个片段），继续记账只会在长文上白占内存。
const MAX_TRACKED_FRAGMENTS: usize = 1024;
/// 参与记账的最小片段长度（字符）。
///
/// 更短的片段既不进分子也不进分母：生成的代码里 `}`、`);` 会合法地重复上百次，不能让它把
/// 重复占比推上去。
const MIN_ACCOUNTED_FRAGMENT_CHARS: usize = 2;
/// 触发短语池判据所需的最小重复质量（字符）。
const MIN_REPEATED_FRAGMENT_CHARS: usize = 2048;
/// 重复质量占记账字符数的百分比下限。
const MIN_REPEATED_FRAGMENT_COVERAGE_PERCENT: usize = 60;
/// 每个去重片段平均至少要出现多少次。
const MIN_REPEATED_FRAGMENT_CONCENTRATION: usize = 4;


/// 错误文本中的稳定前缀，供宿主之外的消费者（如看板扩展）识别这一类失败。
///
/// 看板扩展只能依赖插件系统，无法引用本常量，按字面量匹配并在注释里指向本文件。
pub const DEGENERATE_REPETITION_MARKER: &str = "degenerate repetition detected";

/// 产生退化重复的文本通道。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepetitionStream {
    /// assistant 正文。
    Text,
    /// assistant 思考（reasoning）：模型的自我复读多数发生在这里。
    Thinking,
}

impl std::fmt::Display for RepetitionStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text => f.write_str("正文"),
            Self::Thinking => f.write_str("思考"),
        }
    }
}

/// 触发退化重复的判据。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepetitionRule {
    /// 窗口内去重片段数与平均片段长度都在极小范围，且连续两个窗口成立。
    Window,
    /// 整段响应的重复质量、覆盖率与集中度同时越线。
    PhrasePool,
}

impl std::fmt::Display for RepetitionRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Window => f.write_str("低熵窗口复读"),
            Self::PhrasePool => f.write_str("短语池重排"),
        }
    }
}

/// 一次退化重复的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DegenerateRepetition {
    pub stream: RepetitionStream,
    /// 命中的判据。两条判据的形态不同，调阈值与排障都要靠它。
    pub rule: RepetitionRule,
    /// 记账范围内落在重复片段上的字符数。
    pub repeated_chars: usize,
    /// 记账范围内的字符总数；短语池记账触顶后不再增长，因此是下界。
    pub accounted_chars: usize,
}


/// 单条文本通道的退化重复检测器。
#[derive(Debug)]
pub(crate) struct RepetitionGuard {
    stream: RepetitionStream,
    /// 滑动窗口内的片段，按进入顺序排列。
    window: VecDeque<Fragment>,
    /// 窗口内每个片段的出现次数，`len()` 即去重片段数。
    counts: HashMap<Arc<str>, u32>,
    /// 窗口内片段的字符总数。
    total_chars: usize,
    /// 尚未遇到边界的尾部文本。
    pending: String,
    /// `pending` 中已扫描到的字节位置。
    scanned: usize,
    consecutive_hits: u32,
    /// 整段响应的片段记账，判据二的依据。
    ledger: FragmentLedger,
}


#[derive(Debug)]
struct Fragment {
    text: Arc<str>,
    chars: usize,
}

impl RepetitionGuard {
    pub(crate) fn new(stream: RepetitionStream) -> Self {
        Self {
            stream,
            window: VecDeque::new(),
            counts: HashMap::default(),
            total_chars: 0,
            pending: String::new(),
            scanned: 0,
            consecutive_hits: 0,
            ledger: FragmentLedger::default(),
        }
    }

    /// 重试会重放同一段流，历史统计必须一并清空。
    pub(crate) fn reset(&mut self) {
        self.clear_window();
        self.ledger.clear();
        self.pending.clear();
        self.scanned = 0;
    }


    /// 喂入一段增量文本，返回 `Some` 表示检测到退化重复。
    pub(crate) fn observe(&mut self, delta: &str) -> Option<DegenerateRepetition> {
        self.pending.push_str(delta);
        if self.pending.len() > MAX_PENDING_BYTES {
            // 单个片段就超过上限，说明是长文输出而不是退化重复；统计从头开始。
            self.reset();
            return None;
        }

        let mut fragment_start = 0;
        while let Some(end) = next_boundary(&self.pending, &mut self.scanned) {
            let text = self.pending[fragment_start..end].trim();
            if !text.is_empty() {
                // 一个片段同时进两处记账：窗口（判据一）与整段响应的账本（判据二）。
                push_fragment(
                    &mut self.window,
                    &mut self.counts,
                    &mut self.total_chars,
                    text,
                );
                self.ledger.record(text);
            }
            fragment_start = end;
        }
        if fragment_start > 0 {
            self.pending.drain(..fragment_start);
            self.scanned -= fragment_start;
        }
        self.evaluate()
    }

    fn evaluate(&mut self) -> Option<DegenerateRepetition> {
        // 判据二先判：三条护栏同时越线时，它比「窗口里去重片段少」更精确地指出循环形态。
        if self.ledger.tripped() {
            return Some(self.hit(RepetitionRule::PhrasePool));
        }

        if self.window.len() < WINDOW_FRAGMENTS {
            self.consecutive_hits = 0;
            return None;
        }

        let avg_chars = self.total_chars / self.window.len();
        if self.counts.len() > MAX_DISTINCT_FRAGMENTS || avg_chars > MAX_AVG_FRAGMENT_CHARS {
            self.consecutive_hits = 0;
            return None;
        }

        self.consecutive_hits += 1;
        if self.consecutive_hits < CONSECUTIVE_HITS_REQUIRED {
            return None;
        }
        Some(self.hit(RepetitionRule::Window))
    }

    fn hit(&self, rule: RepetitionRule) -> DegenerateRepetition {
        DegenerateRepetition {
            stream: self.stream,
            rule,
            repeated_chars: self.ledger.repeated_chars,
            accounted_chars: self.ledger.chars,
        }
    }


    fn clear_window(&mut self) {
        self.window.clear();
        self.counts.clear();
        self.total_chars = 0;
        self.consecutive_hits = 0;
    }
}

/// 整段响应的片段账本：判据二的依据。
///
/// 只按 64 位哈希记「这个片段见过几次」，不存原文，因此内存上限是
/// [`MAX_TRACKED_FRAGMENTS`] 项，与输出长度无关。
#[derive(Debug, Default)]
struct FragmentLedger {
    /// 片段哈希 → 出现次数。
    seen: HashMap<u64, u32>,
    /// 已记账片段的字符总数。
    chars: usize,
    /// 落在重复片段上的字符数：第 k 次出现记 `len × k`。
    repeated_chars: usize,
    /// 记账片段的出现次数（集中度的分子）。
    occurrences: usize,
    /// 去重片段数（集中度的分母）。
    distinct: usize,
    /// 去重片段数触顶后已停止记账。
    closed: bool,
}

impl FragmentLedger {
    fn record(&mut self, text: &str) {
        if self.closed {
            return;
        }
        let chars = text.chars().count();
        if chars < MIN_ACCOUNTED_FRAGMENT_CHARS {
            return;
        }

        let key = fragment_key(text);
        let Some(occurrences) = self.seen.get_mut(&key) else {
            if self.distinct >= MAX_TRACKED_FRAGMENTS {
                // 触顶即停：见过这么多互不相同的片段，词汇量已经远超任何循环短语池。已积累的
                // 数值保持不动（`accounted_chars` 因此是下界），不再继续记账。
                self.closed = true;
                return;
            }
            self.seen.insert(key, 1);
            self.distinct += 1;
            self.chars += chars;
            self.occurrences += 1;
            return;
        };

        *occurrences += 1;

        self.chars += chars;
        self.occurrences += 1;
        // 第二次出现时两份都算重复（`× 2`）；之后每出现一次再加一份。
        if *occurrences == 2 {
            self.repeated_chars += chars * 2;
        } else {
            self.repeated_chars += chars;
        }
    }

    /// 三条护栏同时越线才算命中。
    fn tripped(&self) -> bool {
        self.repeated_chars >= MIN_REPEATED_FRAGMENT_CHARS
            && self.chars > 0
            && self.repeated_chars * 100 >= self.chars * MIN_REPEATED_FRAGMENT_COVERAGE_PERCENT
            && self.distinct > 0
            && self.occurrences >= self.distinct * MIN_REPEATED_FRAGMENT_CONCENTRATION
    }

    fn clear(&mut self) {
        *self = Self::default();
    }
}

/// 片段只按 64 位哈希记账：账本只问「见过没有」，不存原文；上限千余项，碰撞只会让个别片段
/// 被算成同一个，不足以凑出三条护栏。
fn fragment_key(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = FxHasher::default();
    text.hash(&mut hasher);
    hasher.finish()
}

/// 入窗：维护去重计数与字符总数，超出窗口容量时同步淘汰最旧的片段。
fn push_fragment(
    window: &mut VecDeque<Fragment>,
    counts: &mut HashMap<Arc<str>, u32>,
    total_chars: &mut usize,
    text: &str,
) {
    let chars = text.chars().count();
    let text: Arc<str> = Arc::from(text);
    *counts.entry(Arc::clone(&text)).or_insert(0) += 1;
    *total_chars += chars;
    window.push_back(Fragment { text, chars });

    if window.len() <= WINDOW_FRAGMENTS {
        return;
    }
    let Some(evicted) = window.pop_front() else {
        return;
    };
    if let Entry::Occupied(mut entry) = counts.entry(Arc::clone(&evicted.text)) {
        if *entry.get() > 1 {
            *entry.get_mut() -= 1;
        } else {
            entry.remove();
        }
    }
    *total_chars -= evicted.chars;
}

/// 从 `cursor` 开始找下一个已确定的片段边界，并把 `cursor` 推进到扫描停止处。
///
/// 换行与中文句末标点总是边界；`.` / `!` / `?` 只在后接空白时才算句末，避免把
/// `foo.bar()`、`3.14`、`file.rs` 切碎。句末标点后面还没收到字符时游标停在标点处，
/// 等下一个增量再判定，因此已扫描的文本不会被重复扫描。
fn next_boundary(text: &str, cursor: &mut usize) -> Option<usize> {
    for (offset, ch) in text[*cursor..].char_indices() {
        let index = *cursor + offset;
        let end = index + ch.len_utf8();
        match ch {
            '\n' | '。' | '！' | '？' | '；' | ';' => {
                *cursor = end;
                return Some(end);
            },
            '.' | '!' | '?' => {
                if text[end..].chars().next().is_some_and(char::is_whitespace) {
                    *cursor = end;
                    return Some(end);
                }
                if end == text.len() {
                    *cursor = index;
                    return None;
                }
            },
            _ => {},
        }
    }
    *cursor = text.len();
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guard(stream: RepetitionStream) -> RepetitionGuard {
        RepetitionGuard::new(stream)
    }

    fn feed(guard: &mut RepetitionGuard, text: &str) -> Option<DegenerateRepetition> {
        guard.observe(text)
    }

    fn boundary_of(text: &str) -> Option<usize> {
        let mut cursor = 0;
        next_boundary(text, &mut cursor)
    }

    /// 用户实际遇到的循环：约十来个短句反复轮转。
    #[test]
    fn cycling_short_phrases_are_detected() {
        const PHRASES: [&str; 13] = [
            "Let me output.",
            "OK.",
            "Producing now.",
            "Let me write.",
            "I'll do it.",
            "Now.",
            "OK.",
            "Producing.",
            "Let me write the calls.",
            "OK, here.",
            "Writing.",
            "Let me produce.",
            "OK.",
        ];
        let mut guard = guard(RepetitionStream::Text);
        let mut detected = None;
        for index in 0..80 {
            let line = format!("{}\n", PHRASES[index % PHRASES.len()]);
            if let Some(repetition) = feed(&mut guard, &line) {
                detected = Some(repetition);
                break;
            }
        }
        let repetition = detected.expect("循环短语必须被判定为退化重复");
        assert_eq!(repetition.stream, RepetitionStream::Text);
        assert_eq!(repetition.rule, RepetitionRule::Window);
        assert!(repetition.accounted_chars > 0, "命中必须带上记账体量");
    }

    /// 思考里的复读往往没有换行，只有句末标点。
    #[test]
    fn run_on_thinking_repetition_is_detected() {
        let mut guard = guard(RepetitionStream::Thinking);
        let mut detected = None;
        for _ in 0..80 {
            if let Some(repetition) = feed(&mut guard, "Let me check. Wait, let me reconsider. ") {
                detected = Some(repetition);
                break;
            }
        }
        let repetition = detected.expect("无换行的思考复读必须被判定为退化重复");
        assert_eq!(repetition.stream, RepetitionStream::Thinking);
    }

    #[test]
    fn varied_prose_never_trips_the_guard() {
        let mut guard = guard(RepetitionStream::Text);
        for index in 0..200 {
            let line = format!("这是第 {index} 段有实质内容的说明文字，长度和用词都在变化。\n");
            assert!(
                feed(&mut guard, &line).is_none(),
                "第 {index} 行不应触发退化重复"
            );
        }
    }

    #[test]
    fn varied_long_fragments_are_not_treated_as_degenerate() {
        let mut guard = guard(RepetitionStream::Text);
        for index in 0..80 {
            let line = format!("第 {index} 段：{}\n", "a".repeat(200));
            assert!(
                feed(&mut guard, &line).is_none(),
                "第 {index} 段长文不应触发"
            );
        }
    }

    /// 反复出现的长片段：窗口判据被平均片段长度挡在门外，账本判据接住。
    #[test]
    fn a_repeated_long_fragment_is_caught_by_the_ledger_rule() {
        let mut guard = guard(RepetitionStream::Thinking);
        let line = format!("{}\n", "a".repeat(200));
        let mut detected = None;
        for _ in 0..40 {
            if let Some(repetition) = feed(&mut guard, &line) {
                detected = Some(repetition);
                break;
            }
        }
        let repetition = detected.expect("反复出现的长片段必须被判定为退化重复");
        assert_eq!(repetition.rule, RepetitionRule::PhrasePool);
    }


    #[test]
    fn short_output_does_not_trip_the_guard() {
        let mut guard = guard(RepetitionStream::Text);
        for _ in 0..10 {
            assert!(feed(&mut guard, "OK.\n").is_none());
        }
    }

    #[test]
    fn deltas_split_mid_fragment_are_joined_before_judging() {
        let mut guard = guard(RepetitionStream::Text);
        let mut detected = None;
        for index in 0..80 {
            let phrase = if index % 2 == 0 { "OK." } else { "Producing." };
            if let Some(repetition) = feed(&mut guard, phrase) {
                detected = Some(repetition);
                break;
            }
            if let Some(repetition) = feed(&mut guard, "\n") {
                detected = Some(repetition);
                break;
            }
        }
        assert!(detected.is_some(), "跨增量的半行必须能拼回完整片段");
    }

    /// 代码里的 `foo.bar()`、`3.14` 不能被当成句末。
    #[test]
    fn dots_inside_code_do_not_end_a_fragment() {
        assert_eq!(boundary_of("self.foo.bar()"), None);
        assert_eq!(boundary_of("let x = 3.14;"), Some(13));
        assert_eq!(boundary_of("done. Next"), Some(5));
    }

    /// 句末标点后还没收到字符时，游标停在标点处，不重扫已扫描的文本。
    #[test]
    fn a_trailing_period_waits_for_the_next_delta() {
        let mut cursor = 0;
        assert_eq!(next_boundary("done.", &mut cursor), None);
        assert_eq!(cursor, 4);

        let mut pending = String::from("done.");
        pending.push_str(" Next");
        assert_eq!(next_boundary(&pending, &mut cursor), Some(5));
    }

    /// 单行超长（无边界）时必须线性处理：扫描游标回退会退化成 O(n²)。
    #[test]
    fn a_very_long_line_is_processed_in_linear_time() {
        let mut guard = guard(RepetitionStream::Text);
        for _ in 0..100_000 {
            assert!(feed(&mut guard, "x").is_none());
        }
        // 超长行之后仍能正常识别循环。
        let mut detected = None;
        for _ in 0..80 {
            if let Some(repetition) = feed(&mut guard, "OK.\n") {
                detected = Some(repetition);
                break;
            }
        }
        assert!(detected.is_some(), "重置后必须仍能识别循环");
    }

    /// 宿主之外的消费者（看板扩展）按字面量识别这类失败，前缀必须与错误文案一致。
    #[test]
    fn marker_matches_the_rendered_turn_error() {
        let error = crate::turn_context::TurnError::DegenerateRepetition {
            stream: RepetitionStream::Thinking,
            rule: RepetitionRule::PhrasePool,
            repeated_chars: 4096,
            accounted_chars: 8192,
        };

        assert!(
            error.to_string().starts_with(DEGENERATE_REPETITION_MARKER),
            "错误文案必须以 {DEGENERATE_REPETITION_MARKER:?} 开头，实际为 {error}"
        );
        assert!(
            error.to_string().contains("思考"),
            "文案必须标明通道: {error}"
        );
        assert!(
            error.to_string().contains("短语池重排"),
            "文案必须标明命中的判据: {error}"
        );
    }

    #[test]
    fn reset_clears_history_so_a_retry_starts_fresh() {
        let mut guard = guard(RepetitionStream::Text);
        for _ in 0..(WINDOW_FRAGMENTS * 2) {
            feed(&mut guard, "OK.\n");
        }
        assert!(guard.ledger.repeated_chars > 0, "账本必须已经记下重复");

        guard.reset();
        assert_eq!(guard.ledger.chars, 0);
        assert_eq!(guard.ledger.distinct, 0);
        assert!(feed(&mut guard, "OK.\n").is_none());
    }

    /// 短语池重排：池子比窗口判据允许的去重片段数还大，窗口那条永远看不见它。插件在真实会话里
    /// 量到的形态是 26 个短语、5,569 次出现、覆盖率 0.993、集中度 214。
    #[test]
    fn a_phrase_pool_larger_than_the_window_rule_still_trips() {
        let mut guard = guard(RepetitionStream::Text);
        let mut detected = None;
        'cycles: for _ in 0..40 {
            for index in 0..26 {
                if let Some(repetition) = feed(&mut guard, &format!("短句{index}。\n")) {
                    detected = Some(repetition);
                    break 'cycles;
                }
            }
        }

        let repetition = detected.expect("大短语池必须被判定为退化重复");
        assert_eq!(repetition.rule, RepetitionRule::PhrasePool);
        assert!(
            guard.ledger.distinct > MAX_DISTINCT_FRAGMENTS,
            "池子必须大于窗口判据的去重上限，否则这条测试证明不了账本判据在起作用"
        );
    }

    /// 同一段代码改前 / 改后贴两遍：覆盖率趋近 1.0，集中度却只有 2，不该当成短语池。
    #[test]
    fn quoting_the_same_block_twice_is_not_a_phrase_pool() {
        let mut guard = guard(RepetitionStream::Text);
        let block: String = (0..40)
            .map(|index| format!("    let field_{index} = compute_value(self.input_{index})?;\n"))
            .collect();
        for _ in 0..2 {
            assert!(feed(&mut guard, &block).is_none());
        }

        assert!(guard.ledger.repeated_chars > 0, "账本看得见重复");
        assert_eq!(guard.ledger.distinct, 40);
        assert_eq!(guard.ledger.occurrences, 80);
    }

    /// 生成的代码里 `}` 会合法地重复上百次：短于 2 字符的片段既不进分子也不进分母。
    #[test]
    fn one_character_fragments_never_enter_the_ledger() {
        let mut guard = guard(RepetitionStream::Text);
        for _ in 0..WINDOW_FRAGMENTS {
            feed(&mut guard, "}\n");
        }
        assert_eq!(guard.ledger.chars, 0);
        assert_eq!(guard.ledger.distinct, 0);
    }

    /// 去重片段数触顶后停止记账：长文不该继续占内存，也不该再被判成退化重复。
    #[test]
    fn the_ledger_stops_accounting_once_its_vocabulary_exceeds_the_cap() {
        let mut guard = guard(RepetitionStream::Text);
        for index in 0..=MAX_TRACKED_FRAGMENTS {
            let line = format!("片段 abcdefghijklmnop{index}。\n");
            assert!(
                feed(&mut guard, &line).is_none(),
                "第 {index} 个片段不应触发"
            );
        }
        assert!(guard.ledger.closed, "去重片段数触顶必须停止记账");

        let frozen = guard.ledger.chars;
        for index in 0..100 {
            let line = format!("另一句完全不同的说明文字{index}。\n");
            assert!(feed(&mut guard, &line).is_none());
        }
        assert_eq!(guard.ledger.chars, frozen, "触顶后记账必须冻结");
    }
}
