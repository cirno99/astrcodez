//! 输入区状态行上的会话用量读数。
//!
//! 照搬前端 `ConversationMetricsBar`：读数统一取**最近一次模型请求**，与 provider 账单同口径
//! ——每轮 step 都会重发整段历史，累计 prompt 会随请求数线性膨胀，累计输出也不对应账单里的
//! 任何一行，直接展示会与账单对不上。
//!
//! 没有样本的指标（尚无请求、上下文身份刚变化、缺少计时锚点）不出项，免得出现误导性的 0。
//! token 数按量级缩写，避免状态行被长数字撑开；前端把精确值与累计值放在悬停说明里，这里
//! 暂时只有缩写值——这套 gpui-kit 的浮层只对自带组件开放，给任意元素挂悬停说明要另接一层
//! 覆盖物（见迁移文档「待打平」）。

use astrcode_protocol::http::ConversationMetricsDto;

/// 状态行上的一项。
pub(crate) struct MetricItem {
    pub(crate) label: &'static str,
    pub(crate) value: String,
}

/// 这一行要画哪几项，顺序与前端一致：输入、输出、缓存、上下文、速度。
pub(crate) fn metrics_row(metrics: &ConversationMetricsDto) -> Vec<MetricItem> {
    let mut items = Vec::new();

    // 最近一次请求的输入与命中率同源：没有输入样本时两者都没有。
    let last_prompt = metrics.last_prompt_tokens.filter(|prompt| *prompt > 0);
    if let Some(prompt) = last_prompt {
        let cached = metrics.last_cached_tokens.unwrap_or(0);
        items.push(MetricItem {
            label: "输入",
            value: format_tokens(prompt),
        });
        items.push(MetricItem {
            label: "缓存",
            value: format_percent(cached as f64 / prompt as f64),
        });
    }

    if let Some(output) = metrics.last_output_tokens {
        items.push(MetricItem {
            label: "输出",
            value: format_tokens(output),
        });
    }

    // 上下文占用要有窗口大小才成比例；缺一半就整项不画。
    if let Some((tokens, window)) = context_usage(metrics) {
        items.push(MetricItem {
            label: "上下文",
            value: format_percent(tokens as f64 / window as f64),
        });
    }

    if let Some(speed) = metrics.output_tokens_per_second {
        items.push(MetricItem {
            label: "速度",
            value: format!("{speed:.1} tok/s"),
        });
    }

    items
}

/// 上下文占用与窗口；两者成对出现，且窗口非零。
fn context_usage(metrics: &ConversationMetricsDto) -> Option<(u64, u64)> {
    let tokens = metrics.context_tokens?;
    let window = metrics.model_context_window.filter(|window| *window > 0)?;
    Some((tokens, window))
}

/// token 数按量级缩写。
fn format_tokens(tokens: u64) -> String {
    if tokens < 1_000 {
        tokens.to_string()
    } else if tokens < 1_000_000 {
        format!("{:.1}K", tokens as f64 / 1_000.0)
    } else {
        format!("{:.2}M", tokens as f64 / 1_000_000.0)
    }
}

fn format_percent(ratio: f64) -> String {
    format!("{:.1}%", ratio * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> ConversationMetricsDto {
        ConversationMetricsDto {
            requests: 3,
            prompt_tokens: 10_000,
            cached_tokens: 4_000,
            cache_creation_tokens: 700,
            output_tokens: 250,
            reasoning_output_tokens: 90,
            last_prompt_tokens: Some(2_000),
            last_cached_tokens: Some(1_000),
            last_output_tokens: Some(120),
            last_reasoning_output_tokens: Some(40),
            context_tokens: Some(64_000),
            model_context_window: Some(128_000),
            output_tokens_per_second: Some(12.34),
        }
    }

    fn labels(metrics: &ConversationMetricsDto) -> Vec<&'static str> {
        metrics_row(metrics)
            .into_iter()
            .map(|item| item.label)
            .collect()
    }

    fn row(metrics: &ConversationMetricsDto) -> Vec<(&str, String)> {
        metrics_row(metrics)
            .into_iter()
            .map(|item| (item.label, item.value))
            .collect()
    }

    #[test]
    fn a_full_sample_renders_every_item_in_the_ported_order() {
        assert_eq!(
            row(&metrics()),
            vec![
                ("输入", "2.0K".to_owned()),
                ("缓存", "50.0%".to_owned()),
                ("输出", "120".to_owned()),
                ("上下文", "50.0%".to_owned()),
                ("速度", "12.3 tok/s".to_owned()),
            ]
        );
    }

    #[test]
    fn items_without_a_sample_are_left_out_instead_of_showing_zero() {
        // 还没有任何请求：全部为空，一项都不画。
        let empty = ConversationMetricsDto {
            requests: 0,
            prompt_tokens: 0,
            cached_tokens: 0,
            cache_creation_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            last_prompt_tokens: None,
            last_cached_tokens: None,
            last_output_tokens: None,
            last_reasoning_output_tokens: None,
            context_tokens: None,
            model_context_window: None,
            output_tokens_per_second: None,
        };
        assert!(metrics_row(&empty).is_empty());

        // 只有累计值也不画：读数是「最近一次请求」，累计值这一版没有出口。
        let mut cumulative_only = empty;
        cumulative_only.prompt_tokens = 5_000;
        cumulative_only.cached_tokens = 5_000;
        assert!(metrics_row(&cumulative_only).is_empty());
    }

    #[test]
    fn a_zero_prompt_leaves_out_both_input_and_cache_hit() {
        let mut metrics = metrics();
        metrics.last_prompt_tokens = Some(0);
        let labels = labels(&metrics);
        assert!(!labels.contains(&"输入"));
        assert!(!labels.contains(&"缓存"), "命中率没有分母就不该出现");
        assert!(labels.contains(&"输出"), "其余指标不受影响");
    }

    #[test]
    fn context_needs_both_the_reading_and_a_non_zero_window() {
        let mut missing_window = metrics();
        missing_window.model_context_window = None;
        assert!(!labels(&missing_window).contains(&"上下文"));

        let mut zero_window = metrics();
        zero_window.model_context_window = Some(0);
        assert!(
            !labels(&zero_window).contains(&"上下文"),
            "零窗口当成没有窗口，不能除出 inf"
        );
    }

    #[test]
    fn token_magnitudes_follow_the_ported_thresholds() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(1_000), "1.0K");
        assert_eq!(format_tokens(15_500), "15.5K");
        assert_eq!(format_tokens(999_999), "1000.0K");
        assert_eq!(format_tokens(1_000_000), "1.00M");
        assert_eq!(format_tokens(2_345_678), "2.35M");
    }
}
