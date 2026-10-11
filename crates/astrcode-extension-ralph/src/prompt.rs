//! 每轮注入的提示与任务文件模板。
//!
//! 提示正文用中文，与 `sleep-continue` 的默认续跑文本同一口径：它是模型可见工件，
//! 不是给开发者的注释。

/// 任务文件读入上限。超过就截断并注明，避免每一轮都把整份文件线性塞进上下文。
pub(crate) const MAX_TASK_FILE_BYTES: usize = 32 * 1024;

/// `/ralph start` 在任务文件不存在时写入的模板。
pub(crate) fn task_file_template(name: &str) -> String {
    format!(
        "# {name}\n\n## 目标\n\n（写清楚这件事「做完」的判据）\n\n## 清单\n\n- [ ] 第一步\n- [ ] \
         第二步\n\n## 验证\n\n（写一条外部可重跑的命令，以及它的工作目录与预期产物）\n\n## \
         记录\n\n（每一轮把做了什么、卡在哪里写在这里）\n"
    )
}

pub(crate) struct RoundPrompt<'a> {
    pub(crate) iteration: u32,
    pub(crate) max_iterations: u32,
    pub(crate) task_file: &'a str,
    pub(crate) task_body: &'a str,
    pub(crate) completion_promise: Option<&'a str>,
    pub(crate) truncated: bool,
}

pub(crate) fn render(prompt: &RoundPrompt<'_>) -> String {
    let budget = if prompt.max_iterations == 0 {
        format!("iteration {}（无上限）", prompt.iteration)
    } else {
        format!("iteration {}/{}", prompt.iteration, prompt.max_iterations)
    };
    let mut text = format!(
        "## Ralph Loop — {budget}\n\n### 任务文件 {}\n{}\n",
        prompt.task_file, prompt.task_body
    );
    if prompt.truncated {
        text.push_str(&format!(
            "\n（任务文件超过 {MAX_TASK_FILE_BYTES} \
             字节，以上为截断后的开头；请把已完成的内容从文件里删掉，只留未完成的部分）\n"
        ));
    }

    text.push_str("\n### 本轮要求\n");
    text.push_str("1. 只做任务文件里未完成的一项，做完立刻把进度回写到任务文件（勾选 + 记录）。\n");
    text.push_str(
        "2. 不得只凭清单打勾宣布完成：必须跑一条外部可重跑的验证命令，并把命令、工作目录、\
         产物路径写进任务文件。\n",
    );
    match prompt.completion_promise {
        Some(promise) => text.push_str(&format!(
            "3. 全部完成后，在回复最后一行输出 <promise>{promise}</promise>。\n"
        )),
        None => text.push_str(
            "3. 本循环未配置完成承诺：它只会因迭代上限或熔断停下，\
             因此请把任务文件维护到能反映真实进度。\n",
        ),
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt<'a>(promise: Option<&'a str>, truncated: bool) -> RoundPrompt<'a> {
        RoundPrompt {
            iteration: 3,
            max_iterations: 50,
            task_file: ".ralph/fix-tests.md",
            task_body: "## 目标\n修好测试",
            completion_promise: promise,
            truncated,
        }
    }

    #[test]
    fn render_includes_budget_task_file_and_gate() {
        let text = render(&prompt(Some("DONE"), false));
        assert!(text.contains("iteration 3/50"));
        assert!(text.contains("### 任务文件 .ralph/fix-tests.md"));
        assert!(text.contains("修好测试"));
        assert!(text.contains("外部可重跑"));
        assert!(text.contains("<promise>DONE</promise>"));
    }

    #[test]
    fn render_without_promise_says_loop_is_cap_bounded() {
        let text = render(&prompt(None, false));
        assert!(!text.contains("<promise>"));
        assert!(text.contains("未配置完成承诺"));
    }

    #[test]
    fn render_marks_truncation_and_unbounded_budget() {
        let mut unbounded = prompt(Some("DONE"), true);
        unbounded.max_iterations = 0;
        let text = render(&unbounded);
        assert!(text.contains("无上限"));
        assert!(text.contains("以上为截断后的开头"));
    }
}
