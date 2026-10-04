//! todoWrite 计划列表的推导，对应 Web 前端的 `Chat/tools/todoWrite.ts`。
//!
//! 进度项在工具结果的 `newTodos` 里（扩展 `astrcode-extension-todo-tool` 回填的、真正落盘
//! 的那份），参数里的 `todos` 是模型这一次提交的原文；前者非空时以前者为准，与前端同序。
//! 这里只做 JSON 推导，不碰 gpui——卡片渲染在 `views::chat`。
//!
//! 解析口径照搬前端：`content` / `activeForm` / `status` 三者缺一，这一项整条丢弃。

use astrcode_protocol::http::ConversationBlockDto;
use serde_json::{Map, Value};

use crate::tool_view::string_at;

/// todoWrite 工具的线缆名。
///
/// 与扩展侧同值（`astrcode-extension-todo-tool` 的 `TODO_WRITE_TOOL_NAME`）：内置插件只依赖
/// 插件系统，共享 UI 层不引它的常量，靠这条注释对齐。
const TODO_WRITE_TOOL_NAME: &str = "todoWrite";

/// 进度项的状态；线缆值是 snake_case。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TodoStatus {
    Pending,
    InProgress,
    Completed,
}

impl TodoStatus {
    /// 卡片上的状态文字，对应前端 `buildTodoRenderSpec` 里三个 `status`。
    pub(crate) fn label(self) -> &'static str {
        match self {
            TodoStatus::Pending => "待处理",
            TodoStatus::InProgress => "进行中",
            TodoStatus::Completed => "已完成",
        }
    }
}

/// 一个进度项：`label` 已带上执行者前缀。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TodoItem {
    pub(crate) label: String,
    pub(crate) status: TodoStatus,
}

/// 进度项按状态的分布。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TodoCounts {
    pub(crate) total: usize,
    pub(crate) pending: usize,
    pub(crate) in_progress: usize,
    pub(crate) completed: usize,
}

impl TodoCounts {
    pub(crate) fn of(items: &[TodoItem]) -> Self {
        let mut counts = TodoCounts {
            total: items.len(),
            ..TodoCounts::default()
        };
        for item in items {
            match item.status {
                TodoStatus::Pending => counts.pending += 1,
                TodoStatus::InProgress => counts.in_progress += 1,
                TodoStatus::Completed => counts.completed += 1,
            }
        }
        counts
    }
}

/// 这块是不是 todoWrite 工具调用。
pub(crate) fn is_todo_list(block: &ConversationBlockDto) -> bool {
    matches!(block, ConversationBlockDto::ToolCall { name, .. } if name == TODO_WRITE_TOOL_NAME)
}

/// 有可展示的进度项时才为真。
///
/// 一项都解析不出时前端那次渲染返回 `undefined`，落到通用正文上；这里用同一个判据
/// 决定要不要出卡片。
pub(crate) fn has_items(block: &ConversationBlockDto) -> bool {
    is_todo_list(block) && !items(block).is_empty()
}

/// 解析出的进度项；结果与参数里都没有时是空的。
pub(crate) fn items(block: &ConversationBlockDto) -> Vec<TodoItem> {
    let ConversationBlockDto::ToolCall {
        arguments_json,
        metadata,
        ..
    } = block
    else {
        return Vec::new();
    };

    let written = array_at(metadata.as_ref(), &["newTodos", "new_todos"]);
    let source = if written.is_empty() {
        array_at(arguments_json.as_ref(), &["todos"])
    } else {
        written
    };
    source.iter().filter_map(todo_item).collect()
}

/// 折叠摘要行，对应前端 `todoWriteSummaryLine`；没有进度项时不给摘要。
pub(crate) fn summary(block: &ConversationBlockDto) -> Option<String> {
    let items = items(block);
    if items.is_empty() {
        return None;
    }

    let counts = TodoCounts::of(&items);
    let mut parts = vec!["todoWrite".to_owned()];
    if counts.pending > 0 {
        parts.push(format!("{} pending", counts.pending));
    }
    if counts.in_progress > 0 {
        parts.push(format!("{} in-progress", counts.in_progress));
    }
    if counts.completed > 0 {
        parts.push(format!("{} done", counts.completed));
    }
    Some(parts.join(" · "))
}

fn todo_item(raw: &Value) -> Option<TodoItem> {
    let fields = raw.as_object()?;
    let content = string_at(Some(fields), &["content"]).trim();
    let active_form = string_at(Some(fields), &["activeForm", "active_form"]).trim();
    if content.is_empty() || active_form.is_empty() {
        return None;
    }

    Some(TodoItem {
        label: format!("{}{content}", executor_tag(fields)),
        status: status_of(fields.get("status")?)?,
    })
}

fn status_of(value: &Value) -> Option<TodoStatus> {
    match value.as_str()? {
        "pending" => Some(TodoStatus::Pending),
        "in_progress" => Some(TodoStatus::InProgress),
        "completed" => Some(TodoStatus::Completed),
        _ => None,
    }
}

/// 执行者前缀，对应前端 `executorTag`：`[self] ` / `[agent: reviewer] `。
fn executor_tag(fields: &Map<String, Value>) -> String {
    match string_at(Some(fields), &["executor"]) {
        "self" => "[self] ".to_owned(),
        "agent" => {
            let agent_type = string_at(Some(fields), &["agentType", "agent_type"]);
            if agent_type.is_empty() {
                "[agent] ".to_owned()
            } else {
                format!("[agent: {agent_type}] ")
            }
        },
        _ => String::new(),
    }
}

/// 取第一个存在的数组字段；不存在或不是数组时给空切片。
fn array_at<'a>(value: Option<&'a Value>, keys: &[&str]) -> &'a [Value] {
    let Some(value) = value else {
        return &[];
    };
    for key in keys {
        if let Some(array) = value.get(*key).and_then(Value::as_array) {
            return array;
        }
    }
    &[]
}

#[cfg(test)]
mod tests {
    use astrcode_protocol::http::{ConversationBlockDto, ToolCallStatusDto};
    use serde_json::{Value, json};

    use super::{TodoCounts, TodoStatus, has_items, items, summary};

    fn tool_call(arguments: Value, metadata: Option<Value>) -> ConversationBlockDto {
        ConversationBlockDto::ToolCall {
            id: "call-1".into(),
            name: "todoWrite".into(),
            arguments: arguments.to_string(),
            text: String::new(),
            status: ToolCallStatusDto::Complete,
            metadata,
            approval: None,
            arguments_json: Some(arguments),
        }
    }

    fn item(content: &str, status: &str, executor: &str) -> Value {
        json!({
            "content": content,
            "activeForm": format!("正在{content}"),
            "status": status,
            "executor": executor,
        })
    }

    #[test]
    fn the_written_list_wins_over_the_submitted_arguments() {
        // 落盘的那份才是事实：模型这次提交 3 项、扩展回填 1 项时，卡片显示回填的那份。
        let block = tool_call(
            json!({ "todos": [item("改一", "pending", "self"), item("改二", "pending", "self")] }),
            Some(json!({ "newTodos": [item("改一", "completed", "self")] })),
        );

        let items = items(&block);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "[self] 改一");
        assert_eq!(items[0].status, TodoStatus::Completed);
    }

    #[test]
    fn an_item_missing_content_active_form_or_status_is_dropped() {
        let block = tool_call(
            json!({
                "todos": [
                    { "content": "缺 activeForm", "status": "pending", "executor": "self" },
                    { "content": "  ", "activeForm": "正在做事", "status": "pending", "executor": "self" },
                    { "content": "状态不认识", "activeForm": "正在做事", "status": "blocked", "executor": "self" },
                    item("完整的一项", "in_progress", "self"),
                ]
            }),
            None,
        );

        let items = items(&block);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "[self] 完整的一项");
    }

    #[test]
    fn the_executor_prefix_names_the_subagent() {
        let mut arguments = json!({
            "todos": [
                item("主代理做", "pending", "self"),
                item("子代理做", "pending", "agent"),
                item("无人认领", "pending", "nobody"),
            ]
        });
        arguments["todos"][1]["agentType"] = json!("reviewer");

        let labels: Vec<String> = items(&tool_call(arguments, None))
            .into_iter()
            .map(|item| item.label)
            .collect();
        assert_eq!(
            labels,
            vec![
                "[self] 主代理做".to_owned(),
                "[agent: reviewer] 子代理做".to_owned(),
                "无人认领".to_owned(),
            ]
        );
    }

    #[test]
    fn the_summary_counts_only_the_states_that_are_present() {
        let block = tool_call(
            json!({
                "todos": [
                    item("待办", "pending", "self"),
                    item("在做甲", "in_progress", "self"),
                    item("在做乙", "in_progress", "agent"),
                    item("完成", "completed", "self"),
                ]
            }),
            None,
        );

        assert_eq!(
            summary(&block).as_deref(),
            Some("todoWrite · 1 pending · 2 in-progress · 1 done")
        );
        let counts = TodoCounts::of(&items(&block));
        assert_eq!(
            (
                counts.total,
                counts.pending,
                counts.in_progress,
                counts.completed
            ),
            (4, 1, 2, 1)
        );
    }

    #[test]
    fn a_list_without_usable_items_is_not_a_card() {
        let empty = tool_call(json!({ "todos": [] }), None);
        assert!(!has_items(&empty));
        assert_eq!(summary(&empty), None);

        // 别的工具不该被当成计划列表。
        let other = ConversationBlockDto::ToolCall {
            id: "call-2".into(),
            name: "patch".into(),
            arguments: "{}".into(),
            text: String::new(),
            status: ToolCallStatusDto::Complete,
            metadata: None,
            approval: None,
            arguments_json: Some(json!({})),
        };
        assert!(!has_items(&other));
    }
}
