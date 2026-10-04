//! 斜杠命令的输入侧推导：触发检测、过滤与插入。
//!
//! 纯函数、不依赖 gpui：面板何时开、过滤出哪些命令、插入后光标落在哪里，都能脱窗口测试。
//! 口径照搬前端 `InputBar.tsx` 的 `findSlashTrigger` / `updateArgTrigger`。

use astrcode_protocol::{
    http::SlashCommandInfoDto,
    wire::{CommandExecutionDto, SessionCommandKindDto},
};

/// 技能扩展的 id：命令面板按它把技能与插件分成两组。
pub(crate) const SKILL_EXTENSION_ID: &str = "astrcode-skill";

/// `/` 触发的命令面板上下文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SlashTrigger {
    /// `/` 的字节位置。
    pub start: usize,
    /// 光标（也就是触发区间的右端）的字节位置。
    pub end: usize,
    /// `/` 之后到光标之间的查询串。
    pub query: String,
}

/// `/name args` 里的参数补全上下文。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ArgTrigger {
    /// 命令名（已按注册表里的大小写）。
    pub command_name: String,
    /// 参数区间的左端：命令名之后的空白之后。
    pub argument_start: usize,
    /// 参数区间的右端（光标）。
    pub cursor: usize,
}

/// 在当前行找 `/` 触发上下文；返回 `None` 表示不该开面板。
pub(crate) fn find_slash_trigger(text: &str, caret: usize) -> Option<SlashTrigger> {
    let caret = clamp_caret(text, caret);
    let line_start = line_start(text, caret);
    let segment = &text[line_start..caret];
    let slash = segment.rfind('/')?;

    // 只认行首或空格之后的 `/`：路径、分数之类的普通文本不触发。
    if slash != 0 && !segment[..slash].ends_with(' ') {
        return None;
    }
    let query = &segment[slash + 1..];
    // `/` 之后出现空白就说明参数已经开始，命令面板让位给参数补全。
    if query.chars().any(char::is_whitespace) {
        return None;
    }

    Some(SlashTrigger {
        start: line_start + slash,
        end: caret,
        query: query.to_owned(),
    })
}

/// 在当前行找 `/name args` 的参数补全上下文；命令不认识或不支持补全时返回 `None`。
pub(crate) fn find_arg_trigger(
    text: &str,
    caret: usize,
    commands: &[SlashCommandInfoDto],
) -> Option<ArgTrigger> {
    let caret = clamp_caret(text, caret);
    let line_start = line_start(text, caret);
    let rest = text[line_start..caret].strip_prefix('/')?;

    let name_len = rest.find(char::is_whitespace)?;
    let name = &rest[..name_len];
    if name.is_empty() {
        return None;
    }
    let after_name = &rest[name_len..];
    // 命令名与参数之间至少要有一个空白；`\s+` 一律吃掉，参数从第一个非空白字符算起。
    let gap = after_name.len() - after_name.trim_start().len();
    if gap == 0 {
        return None;
    }

    let command = commands
        .iter()
        .find(|command| command.name.eq_ignore_ascii_case(name))?;
    command.argument_completions.then(|| ArgTrigger {
        command_name: command.name.clone(),
        argument_start: line_start + 1 + name_len + gap,
        cursor: caret,
    })
}

/// 命令面板的可见项：空查询给全部，否则按名字或描述做大小写不敏感的子串匹配。
pub(crate) fn visible_commands<'a>(
    commands: &'a [SlashCommandInfoDto],
    query: &str,
) -> Vec<&'a SlashCommandInfoDto> {
    if query.is_empty() {
        return commands.iter().collect();
    }
    let query = query.to_lowercase();
    commands
        .iter()
        .filter(|command| {
            command.name.to_lowercase().contains(&query)
                || command.description.to_lowercase().contains(&query)
        })
        .collect()
}

/// 选中一条命令：把触发区间换成 `/name `，返回新文本与新的光标位置。
pub(crate) fn slash_insert(text: &str, trigger: &SlashTrigger, name: &str) -> (String, usize) {
    let insert = format!("/{name} ");
    let mut next = String::with_capacity(text.len() + insert.len());
    next.push_str(&text[..trigger.start]);
    next.push_str(&insert);
    next.push_str(&text[trigger.end..]);
    (next, trigger.start + insert.len())
}

/// 选中一条参数补全：把参数区间换成候选文本，返回新文本与新的光标位置。
pub(crate) fn arg_insert(text: &str, trigger: &ArgTrigger, insert: &str) -> (String, usize) {
    let mut next = String::with_capacity(text.len() + insert.len());
    next.push_str(&text[..trigger.argument_start]);
    next.push_str(insert);
    next.push_str(&text[trigger.cursor..]);
    (next, trigger.argument_start + insert.len())
}

/// 命令是不是技能：面板据此换分组标题与图标。
pub(crate) fn is_skill_command(extension_id: &str) -> bool {
    extension_id == SKILL_EXTENSION_ID
}

/// 文本是不是 `/compact`。
///
/// 压缩会换掉整篇转录，所以提交路径要按名字把它单独挑出来，收到 `Handled` 后重载会话。
pub(crate) fn is_compact_command(text: &str, commands: &[SlashCommandInfoDto]) -> bool {
    let Some(name) = command_name(text) else {
        return false;
    };
    commands.iter().any(|command| {
        command.name.eq_ignore_ascii_case(name)
            && command.execution == CommandExecutionDto::Host(SessionCommandKindDto::CompactSession)
    })
}

/// 文本是不是已注册的扩展/内置斜杠命令。
///
/// 忙的时候这类输入不进待发队列：它们多半由宿主直接处理（如 `/compact`），排队只会让
/// 「现在就想做的事」等一整个 turn。与前端 `isRegisteredSlashCommand` 同判据。
pub(crate) fn is_registered_command(text: &str, commands: &[SlashCommandInfoDto]) -> bool {
    let Some(name) = command_name(text) else {
        return false;
    };
    commands
        .iter()
        .any(|command| command.name.eq_ignore_ascii_case(name))
}

/// 取 `/name args` 里的名字；不是斜杠命令或没有名字时返回 `None`。
fn command_name(text: &str) -> Option<&str> {
    let body = text.trim().strip_prefix('/')?.trim_start();
    let name = body.split(char::is_whitespace).next().unwrap_or_default();
    (!name.is_empty()).then_some(name)
}

/// 光标所在行的起始字节位置。
fn line_start(text: &str, caret: usize) -> usize {
    text[..caret].rfind('\n').map_or(0, |index| index + 1)
}

/// 把光标夹到合法字节边界：引擎给的位置越界或落在字符中间时退回前一个边界。
fn clamp_caret(text: &str, caret: usize) -> usize {
    let mut caret = caret.min(text.len());
    while !text.is_char_boundary(caret) {
        caret -= 1;
    }
    caret
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(name: &str, description: &str, completions: bool) -> SlashCommandInfoDto {
        SlashCommandInfoDto {
            name: name.to_owned(),
            extension_id: "astrcode-extension-test".to_owned(),
            description: description.to_owned(),
            args_schema: None,
            needs_argument: completions,
            requires_idle: false,
            argument_completions: completions,
            priority: 0,
            availability: astrcode_protocol::wire::CommandAvailabilityDto::AllTransports,
            execution: CommandExecutionDto::Extension,
        }
    }

    fn compact() -> SlashCommandInfoDto {
        SlashCommandInfoDto {
            execution: CommandExecutionDto::Host(SessionCommandKindDto::CompactSession),
            ..command("compact", "压缩当前会话", false)
        }
    }

    #[test]
    fn slash_at_line_start_opens_the_panel() {
        let trigger = find_slash_trigger("/comp", 5).expect("trigger");
        assert_eq!(trigger.start, 0);
        assert_eq!(trigger.end, 5);
        assert_eq!(trigger.query, "comp");
    }

    #[test]
    fn slash_after_a_space_opens_the_panel() {
        let trigger = find_slash_trigger("看下 /comp", "看下 /comp".len()).expect("trigger");
        assert_eq!(trigger.query, "comp");
    }

    #[test]
    fn slash_inside_a_word_is_plain_text() {
        assert!(find_slash_trigger("foo/bar", 7).is_none());
    }

    #[test]
    fn whitespace_after_the_slash_closes_the_panel() {
        assert!(find_slash_trigger("/comp act", 9).is_none());
    }

    #[test]
    fn only_the_current_line_counts() {
        let text = "/compact\n普通文本";
        assert!(find_slash_trigger(text, text.len()).is_none());
        let trigger = find_slash_trigger("/comp\n普通文本", 5).expect("trigger");
        assert_eq!(trigger.query, "comp");
    }

    #[test]
    fn out_of_range_caret_does_not_panic() {
        let text = "/多字节";
        let trigger = find_slash_trigger(text, 99).expect("trigger");
        assert_eq!(trigger.end, text.len());
        assert_eq!(trigger.query, "多字节");
        // 落在「多」字中间：退回字符边界，`/` 之后什么都没有。
        assert_eq!(find_slash_trigger(text, 2).expect("trigger").query, "");
    }

    #[test]
    fn filtering_matches_name_or_description_case_insensitively() {
        let commands = vec![
            command("goal", "目标管理", false),
            command("Review", "审查", false),
        ];
        let names = |query: &str| {
            visible_commands(&commands, query)
                .iter()
                .map(|command| command.name.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(""), ["goal", "Review"]);
        assert_eq!(names("GO"), ["goal"]);
        assert_eq!(names("审查"), ["Review"]);
        assert!(names("没有这个").is_empty());
    }

    #[test]
    fn arg_trigger_needs_the_whitespace_after_the_name() {
        let commands = vec![command("goal", "目标管理", true)];
        assert!(find_arg_trigger("/goal", 5, &commands).is_none());
        let trigger = find_arg_trigger("/goal 修好", "/goal 修好".len(), &commands).unwrap();
        assert_eq!(trigger.command_name, "goal");
        assert_eq!(trigger.argument_start, 6);
    }

    #[test]
    fn arg_trigger_skips_the_whitespace_run() {
        let commands = vec![command("goal", "目标管理", true)];
        let text = "/goal   修好";
        let trigger = find_arg_trigger(text, text.len(), &commands).expect("trigger");
        assert_eq!(trigger.argument_start, 8);
    }

    #[test]
    fn arg_trigger_ignores_unknown_or_plain_commands() {
        let plain = vec![command("compact", "压缩", false)];
        assert!(find_arg_trigger("/compact ", 9, &plain).is_none());
        let commands = vec![command("goal", "目标管理", true)];
        assert!(find_arg_trigger("/nope ", 6, &commands).is_none());
        assert!(find_arg_trigger("普通文本 ", 10, &commands).is_none());
    }

    #[test]
    fn slash_insert_replaces_the_query_and_leaves_the_caret_after_the_space() {
        let trigger = find_slash_trigger("/comp 后面的文本", 5).expect("trigger");
        let (text, caret) = slash_insert("/comp 后面的文本", &trigger, "compact");
        assert_eq!(text, "/compact  后面的文本");
        assert_eq!(caret, 9);
    }

    #[test]
    fn arg_insert_replaces_only_the_argument() {
        let commands = vec![command("goal", "目标管理", true)];
        let text = "/goal 旧值";
        let trigger = find_arg_trigger(text, text.len(), &commands).expect("trigger");
        let (next, caret) = arg_insert(text, &trigger, "新值");
        assert_eq!(next, "/goal 新值");
        assert_eq!(caret, next.len());
    }

    #[test]
    fn only_the_host_compact_command_is_compact() {
        let commands = vec![compact(), command("compact", "同名的扩展命令", false)];
        assert!(is_compact_command("/compact", &commands));
        assert!(is_compact_command("  /compact 3 ", &commands));
        assert!(!is_compact_command("普通文本", &commands));
        assert!(!is_compact_command("/", &commands));
        let without_host = vec![command("compact", "压缩", false)];
        assert!(!is_compact_command("/compact", &without_host));
    }

    #[test]
    fn a_registered_command_is_recognised_by_name_case_insensitively() {
        let commands = vec![
            command("compact", "压缩", false),
            command("ralph", "循环", true),
        ];

        assert!(is_registered_command("/compact", &commands));
        assert!(is_registered_command("  /COMPACT 现在 ", &commands));
        assert!(is_registered_command("/ralph 3 次", &commands));
        assert!(!is_registered_command("/unknown", &commands));
        assert!(!is_registered_command("普通文本", &commands));
        assert!(!is_registered_command("/", &commands));
    }

    #[test]
    fn skills_are_grouped_by_extension_id() {
        assert!(is_skill_command(SKILL_EXTENSION_ID));
        assert!(!is_skill_command("astrcode-extension-goal"));
    }
}
