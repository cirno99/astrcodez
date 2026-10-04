//! `astrcode-kanban` 扩展的线缆形状。
//!
//! 这个形状归扩展所有，不进 `astrcode-protocol`：扩展是自己的契约所有者，它加字段时
//! 不该让宿主解码失败。因此这里不用 `deny_unknown_fields`，缺省字段取扩展侧同名的
//! 默认语义。
//!
//! 「内部枚举与线缆取值分离」在这里不适用：本层的唯一职责就是线缆。列顺序、落点、
//! 日历分桶这些领域语义在 [`super`]。

use serde::{Deserialize, Serialize};

/// 看板扩展的 id，同时是它注册路由的前缀。
pub const EXTENSION_ID: &str = "astrcode-kanban";

/// 卡片所在的列；取值与扩展的 `CardColumnDto` 一一对应。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardColumn {
    Backlog,
    Ready,
    Analyzing,
    Implementing,
    Done,
    Blocked,
}

impl CardColumn {
    /// 六列在页面上的固定顺序。
    pub const ALL: [Self; 6] = [
        Self::Backlog,
        Self::Ready,
        Self::Analyzing,
        Self::Implementing,
        Self::Done,
        Self::Blocked,
    ];

    /// 运行中的列由扩展独占写入，用户写进去只会拿到 400。
    pub fn is_running(self) -> bool {
        matches!(self, Self::Analyzing | Self::Implementing)
    }
}

/// 一张需求卡片。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Card {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub column: CardColumn,
    pub working_dir: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub attempt: u32,
    #[serde(default)]
    pub note: Option<String>,
    /// 卡片在日历上的归属日；空串表示归属日未知（旧数据），归入「未排期」。
    #[serde(default)]
    pub date: String,
    pub created_at: String,
    pub updated_at: String,
}

/// `GET /board` 的响应封套。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoardResponse {
    pub cards: Vec<Card>,
}

/// `POST /cards` 的请求体。
///
/// 省略的字段由扩展自己补默认值：`column` 取待办、`workingDir` 取扩展配置的默认目录、
/// `date` 取创建当天。宿主不重复这些默认值，否则两边迟早会不一致。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCardRequest {
    pub title: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<CardColumn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

/// `PATCH /cards/{cardId}` 的请求体：只发要改的字段。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCardRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<CardColumn>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

/// 文件夹选择器里的一层子目录。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
}

/// `POST /directories` 的响应。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    /// 实际列举的目录路径。
    pub path: String,
    /// 上级目录；已经是根目录时为 `None`。
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub entries: Vec<DirectoryEntry>,
    /// 是否因为条目上限而截断；截断时列表不完整，用户仍可手输路径。
    #[serde(default)]
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_decodes_the_extensions_wire_shape() {
        let card: Card = serde_json::from_value(serde_json::json!({
            "id": "c1",
            "title": "接入看板",
            "body": "正文",
            "column": "analyzing",
            "workingDir": "/w/project",
            "sessionId": "s1",
            "attempt": 2,
            "note": "备注",
            "date": "2026-03-07",
            "createdAt": "2026-03-07T01:02:03+00:00",
            "updatedAt": "2026-03-07T01:02:03+00:00"
        }))
        .expect("字段齐全的卡片必须可解码");

        assert_eq!(card.column, CardColumn::Analyzing);
        assert_eq!(card.working_dir, "/w/project");
        assert_eq!(card.session_id.as_deref(), Some("s1"));
    }

    /// 扩展省略可选字段时宿主仍要能读：`sessionId` / `note` 只在有值时序列化。
    #[test]
    fn card_tolerates_the_optional_fields_being_absent() {
        let card: Card = serde_json::from_value(serde_json::json!({
            "id": "c1",
            "title": "t",
            "column": "backlog",
            "workingDir": "/w/project",
            "createdAt": "2026-03-07T01:02:03+00:00",
            "updatedAt": "2026-03-07T01:02:03+00:00"
        }))
        .expect("省略可选字段的卡片必须可解码");

        assert_eq!(card.session_id, None);
        assert_eq!(card.attempt, 0);
        assert!(card.date.is_empty(), "缺 date 的旧卡片按空串归入未排期");
    }

    /// 扩展加字段不该让宿主解码失败——这是「形状归扩展所有」的具体代价。
    #[test]
    fn card_ignores_fields_the_host_does_not_know_yet() {
        let card: Card = serde_json::from_value(serde_json::json!({
            "id": "c1",
            "title": "t",
            "column": "done",
            "workingDir": "/w/project",
            "createdAt": "2026-03-07T01:02:03+00:00",
            "updatedAt": "2026-03-07T01:02:03+00:00",
            "errorRetries": 3
        }))
        .expect("未知字段必须被忽略");

        assert_eq!(card.column, CardColumn::Done);
    }

    #[test]
    fn create_request_omits_the_fields_the_extension_defaults() {
        let request = CreateCardRequest {
            title: "t".into(),
            body: String::new(),
            column: None,
            working_dir: None,
            date: None,
        };
        let value = serde_json::to_value(&request).unwrap();

        assert_eq!(value, serde_json::json!({ "title": "t", "body": "" }));
    }

    #[test]
    fn update_request_sends_only_the_changed_fields() {
        let request = UpdateCardRequest {
            column: Some(CardColumn::Blocked),
            date: Some("2026-03-08".into()),
            ..UpdateCardRequest::default()
        };
        let value = serde_json::to_value(&request).unwrap();

        assert_eq!(
            value,
            serde_json::json!({ "column": "blocked", "date": "2026-03-08" })
        );
    }

    #[test]
    fn directory_listing_decodes_including_a_root_without_parent() {
        let listing: DirectoryListing = serde_json::from_value(serde_json::json!({
            "path": "/",
            "parent": null,
            "entries": [{ "name": "etc", "path": "/etc" }],
            "truncated": false
        }))
        .expect("根目录的列举必须可解码");

        assert_eq!(listing.parent, None);
        assert_eq!(listing.entries.len(), 1);
        assert!(!listing.truncated);
    }

    #[test]
    fn running_columns_are_exactly_the_two_the_automation_owns() {
        let running: Vec<CardColumn> = CardColumn::ALL
            .into_iter()
            .filter(|column| column.is_running())
            .collect();
        assert_eq!(
            running,
            vec![CardColumn::Analyzing, CardColumn::Implementing]
        );
    }
}
